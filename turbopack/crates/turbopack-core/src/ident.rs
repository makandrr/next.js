use std::{
    fmt::{Debug, Write},
    hash::Hash,
    mem::take,
    ops::Deref,
    sync::Arc,
};

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use turbo_rcstr::RcStr;
use turbo_tasks::{NonLocalValue, TaskInput, Vc, trace::TraceRawVcs};
use turbo_tasks_fs::FileSystemPath;
use turbo_tasks_hash::{DeterministicHash, Xxh3Hash64Hasher, encode_hex, hash_xxh3_hash64};

use crate::resolve::ModulePart;

/// A layer identifies a distinct part of the module graph.
///
/// Construct a layer with `new_layer!` macro
#[derive(
    Copy,
    Clone,
    TaskInput,
    Hash,
    Debug,
    DeterministicHash,
    Eq,
    PartialEq,
    TraceRawVcs,
    Serialize,
    Deserialize,
    NonLocalValue,
)]
pub struct Layer {
    id: u8,
}

/// A list of all layers sorted by name
static LAYERS: Lazy<Vec<LayerRegistration>> = Lazy::new(|| {
    let mut all_layers: Vec<_> = inventory::iter::<LayerRegistration>().copied().collect();
    all_layers.sort_by_key(|registration| registration.name);
    let mut prev: Option<&LayerRegistration> = None;
    for registration in all_layers.iter() {
        if let Some(prev) = prev
            && prev.name == registration.name
        {
            panic!(
                "duplicate layer definition, names should be unique: {prev:?}, {registration:?}"
            );
        }
        prev = Some(registration);
    }
    assert!(all_layers.len() <= u8::MAX as usize);
    all_layers
});

impl Layer {
    #[doc(hidden)]
    pub fn new(name: &'static str) -> Self {
        debug_assert!(!name.is_empty());
        let id = match LAYERS.binary_search_by_key(&name, |registration| registration.name) {
            // Safety: we know that the length of the layers is less than u8::MAX due to the assert
            // in LAYERS above
            Ok(id) => id as u8,
            Err(_) => panic!("layer not found: {name}, did you forget to call `new_layer!`?"),
        };

        Self { id }
    }

    /// Returns a user friendly name for this layer
    pub fn user_friendly_name(&self) -> &'static str {
        let r = &LAYERS[self.id as usize];
        r.user_friendly_name.unwrap_or(r.name)
    }

    pub fn name(&self) -> &'static str {
        LAYERS[self.id as usize].name
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct LayerRegistration {
    pub name: &'static str,
    pub user_friendly_name: Option<&'static str>,
}

inventory::collect!(LayerRegistration);

#[macro_export]
macro_rules! new_layer {
    ($var:ident, $name:expr, $user_friendly_name:expr) => {
        turbo_tasks::macro_helpers::inventory_submit!($crate::ident::LayerRegistration {
            name: $name,
            user_friendly_name: Some($user_friendly_name)
        });
        static $var: ::turbo_tasks::macro_helpers::Lazy<$crate::ident::Layer> =
            ::turbo_tasks::macro_helpers::Lazy::new(|| $crate::ident::Layer::new($name));
    };
    ($var:ident, $name:expr) => {
        turbo_tasks::macro_helpers::inventory_submit!($crate::ident::LayerRegistration {
            name: $name,
            user_friendly_name: None
        });
        static $var: ::turbo_tasks::macro_helpers::Lazy<$crate::ident::Layer> =
            ::turbo_tasks::macro_helpers::Lazy::new(|| $crate::ident::Layer::new($name));
    };
}

// AssetIdent is wrapped in Arc to make cloning extremely cheap (8 bytes + atomic increment)
// In large builds there are tens of thousands of AssetIdents that get cloned frequently.
// The tradeoff is one extra pointer indirection on field access, but this is negligible
// compared to the clone performance improvement.

#[derive(
    Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, TraceRawVcs, NonLocalValue, TaskInput,
)]
pub struct AssetIdentInner {
    /// The primary path of the asset
    pub path: FileSystemPath,
    /// The query string of the asset this is either the empty string or a query string that starts
    /// with a `?` (e.g. `?foo=bar`)
    pub query: RcStr,
    /// The fragment of the asset, this is either the empty string or a fragment string that starts
    /// with a `#` (e.g. `#foo`)
    pub fragment: RcStr,
    /// The assets that are nested in this asset
    /// Formatted as a sequence of `key => asset` pairs
    pub assets: RcStr,
    /// The modifiers of this asset (e.g. `client chunks`) as a comma separated list
    pub modifiers: RcStr,
    /// The parts of the asset that are (ECMAScript) modules a list of <part> separated by
    /// whitespace.
    pub parts: RcStr,
    /// The asset layer the asset was created from.
    pub layer: Option<Layer>,
    /// The MIME content type, if this asset was created from a data URL.
    pub content_type: Option<RcStr>,
}

#[turbo_tasks::value(shared, eq = "manual")]
#[derive(Clone)]
pub struct AssetIdent(pub(crate) Arc<AssetIdentInner>);

impl Deref for AssetIdent {
    type Target = AssetIdentInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Manual implementations for Arc wrapper
impl Debug for AssetIdent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Hash for AssetIdent {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for AssetIdent {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for AssetIdent {}

impl TaskInput for AssetIdent {
    fn is_transient(&self) -> bool {
        false
    }
}

impl AssetIdent {
    fn check_non_empty_and_no_commas(modifier: &RcStr) {
        debug_assert!(!modifier.is_empty(), "modifiers cannot be empty.");
        debug_assert!(!modifier.contains(","), "modifiers cannot contain commas.");
    }
    pub fn add_modifier(&mut self, modifier: RcStr) {
        if self.0.modifiers.is_empty() {
            Self::check_non_empty_and_no_commas(&modifier);
            let inner = Arc::make_mut(&mut self.0);
            inner.modifiers = modifier;
            return;
        }
        self.add_modifiers(std::iter::once(modifier));
    }

    pub fn add_modifiers(&mut self, new_modifiers: impl IntoIterator<Item = RcStr>) {
        let inner = Arc::make_mut(&mut self.0);
        let mut modifiers = take(&mut inner.modifiers).into_owned();
        for modifier in new_modifiers {
            Self::check_non_empty_and_no_commas(&modifier);
            if !modifiers.is_empty() {
                modifiers.push_str(", ");
            }
            modifiers.push_str(&modifier);
        }
        inner.modifiers = RcStr::from(modifiers);
    }

    pub async fn add_asset(&mut self, key: RcStr, asset: &AssetIdent) -> Result<()> {
        let inner = Arc::make_mut(&mut self.0);
        let mut assets = take(&mut inner.assets).into_owned();
        if !assets.is_empty() {
            assets.push_str(", ");
        }
        write!(
            assets,
            " {key} => {asset}",
            asset = asset.value_to_string().await?
        )
        .expect("failed to write to assets");

        inner.assets = RcStr::from(assets);
        Ok(())
    }
    pub async fn add_assets(&mut self, items: Vec<(RcStr, Vc<AssetIdent>)>) -> Result<()> {
        debug_assert!(!items.is_empty(), "assets cannot be empty.");
        let inner = Arc::make_mut(&mut self.0);
        let mut assets = take(&mut inner.assets).into_owned();
        for (key, asset) in items {
            if !assets.is_empty() {
                assets.push_str(", ");
            }
            write!(
                assets,
                " {key} => {asset}",
                asset = asset.await?.value_to_string().await?
            )
            .expect("failed to write to assets");
        }
        inner.assets = RcStr::from(assets);
        Ok(())
    }

    pub fn add_part(&mut self, part: ModulePart) {
        if matches!(part, ModulePart::Facade) {
            // facade is not included in ident as switching between facade and non-facade
            // shouldn't change the ident
            return;
        }
        if self.0.parts.is_empty() {
            let inner = Arc::make_mut(&mut self.0);
            inner.parts = RcStr::from(part.to_string());
            return;
        }
        self.add_parts(std::iter::once(part));
    }

    pub fn add_parts(&mut self, new_parts: impl IntoIterator<Item = ModulePart>) {
        let inner = Arc::make_mut(&mut self.0);
        let mut parts = take(&mut inner.parts).into_owned();
        for part in new_parts {
            if matches!(part, ModulePart::Facade) {
                // facade is not included in ident as switching between facade and non-facade
                // shouldn't change the ident
                continue;
            }
            if !parts.is_empty() {
                parts.push(' ');
            }
            parts.push_str(&part.to_string());
        }
        inner.parts = RcStr::from(parts);
    }

    pub async fn rename_as_ref(&mut self, pattern: &str) -> Result<()> {
        let inner = Arc::make_mut(&mut self.0);
        let root = inner.path.root().await?;
        inner.path = root.join(&pattern.replace('*', &inner.path.path))?;
        Ok(())
    }

    /// Sets the query string of the asset
    pub fn set_query(&mut self, query: RcStr) {
        debug_assert!(!query.is_empty(), "query cannot be empty.");
        let inner = Arc::make_mut(&mut self.0);
        inner.query = query;
    }

    /// Sets the fragment of the asset
    pub fn set_fragment(&mut self, fragment: RcStr) {
        debug_assert!(!fragment.is_empty(), "fragment cannot be empty.");
        let inner = Arc::make_mut(&mut self.0);
        inner.fragment = fragment;
    }

    /// Sets the content type of the asset
    pub fn set_content_type(&mut self, content_type: RcStr) {
        debug_assert!(!content_type.is_empty(), "content type cannot be empty.");
        let inner = Arc::make_mut(&mut self.0);
        inner.content_type = Some(content_type);
    }

    /// Sets the layer of the asset
    pub fn set_layer(&mut self, layer: Layer) {
        let inner = Arc::make_mut(&mut self.0);
        inner.layer = Some(layer);
    }

    /// Sets the path of the asset
    pub fn set_path(&mut self, path: FileSystemPath) {
        let inner = Arc::make_mut(&mut self.0);
        inner.path = path;
    }

    /// Creates an [AssetIdent] from a [FileSystemPath]
    pub fn from_path(path: FileSystemPath) -> Self {
        Self(Arc::new(AssetIdentInner {
            path,
            query: RcStr::default(),
            fragment: RcStr::default(),
            assets: RcStr::default(),
            modifiers: RcStr::default(),
            parts: RcStr::default(),
            layer: None,
            content_type: None,
        }))
    }

    pub async fn path(self: Vc<Self>) -> Result<FileSystemPath> {
        Ok(self.await?.path.clone())
    }
}

impl AssetIdent {
    /// Computes a unique output asset name for the given asset identifier.
    /// TODO(alexkirsz) This is `turbopack-browser` specific, as
    /// `turbopack-nodejs` would use a content hash instead. But for now
    /// both are using the same name generation logic.
    pub async fn output_name(
        &self,
        context_path: FileSystemPath,
        prefix: Option<RcStr>,
        expected_extension: RcStr,
    ) -> Result<String> {
        debug_assert!(
            expected_extension.starts_with("."),
            "the extension should include the leading '.', got '{expected_extension}'"
        );
        // TODO(PACK-2140): restrict character set to A–Za–z0–9-_.~'()
        // to be compatible with all operating systems + URLs.

        // For clippy -- This explicit deref is necessary
        let path = &self.path;
        let mut name = if let Some(inner) = context_path.get_path_to(path) {
            clean_separators(inner)
        } else {
            clean_separators(&self.path.value_to_string().await?)
        };
        let removed_extension = name.ends_with(&*expected_extension);
        if removed_extension {
            name.truncate(name.len() - expected_extension.len());
        }
        // This step ensures that leading dots are not preserved in file names. This is
        // important as some file servers do not serve files with leading dots (e.g.
        // Next.js).
        let mut name = clean_additional_extensions(&name);
        if let Some(prefix) = prefix {
            name = format!("{prefix}-{name}");
        }

        let default_modifier = match expected_extension.as_str() {
            ".js" => Some("ecmascript"),
            ".css" => Some("css"),
            _ => None,
        };

        let mut hasher = Xxh3Hash64Hasher::new();
        let mut has_hash = false;
        let query = &self.query;
        let fragment = &self.fragment;
        let assets = &self.assets;
        let modifiers = &self.modifiers;
        let parts = &self.parts;
        let layer = &self.layer;
        let content_type = &self.content_type;
        if !query.is_empty() {
            0_u8.deterministic_hash(&mut hasher);
            query.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if !fragment.is_empty() {
            1_u8.deterministic_hash(&mut hasher);
            fragment.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if !assets.is_empty() {
            2_u8.deterministic_hash(&mut hasher);
            assets.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if !modifiers.is_empty() {
            // TODO: document why it is important to strip the default modifier from the hash
            for modifier in modifiers.split(", ") {
                if let Some(default_modifier) = default_modifier
                    && modifier == default_modifier
                {
                    continue;
                }
                3_u8.deterministic_hash(&mut hasher);
                modifier.deterministic_hash(&mut hasher);
                has_hash = true;
            }
        }
        if !parts.is_empty() {
            4_u8.deterministic_hash(&mut hasher);
            parts.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if let Some(layer) = layer {
            5_u8.deterministic_hash(&mut hasher);
            layer.deterministic_hash(&mut hasher);
            has_hash = true;
        }
        if let Some(content_type) = content_type {
            6_u8.deterministic_hash(&mut hasher);
            content_type.deterministic_hash(&mut hasher);
            has_hash = true;
        }

        if has_hash {
            let hash = encode_hex(hasher.finish());
            let truncated_hash = &hash[..8];
            write!(name, "_{truncated_hash}")?;
        }

        // Location in "path" where hashed and named parts are split.
        // Everything before i is hashed and after i named.
        let mut i = 0;
        static NODE_MODULES: &str = "_node_modules_";
        if let Some(j) = name.rfind(NODE_MODULES) {
            i = j + NODE_MODULES.len();
        }
        const MAX_FILENAME: usize = 80;
        if name.len() - i > MAX_FILENAME {
            i = name.len() - MAX_FILENAME;
            if let Some(j) = name[i..].find('_')
                && j < 20
            {
                i += j + 1;
            }
        }
        if i > 0 {
            let hash = encode_hex(hash_xxh3_hash64(&name.as_bytes()[..i]));
            let truncated_hash = &hash[..5];
            name = format!("{}_{}", truncated_hash, &name[i..]);
        }
        // We need to make sure that `.json` and `.json.js` doesn't end up with the same
        // name. So when we add an extra extension when want to mark that with a "._"
        // suffix.
        if !removed_extension {
            name += "._";
        }
        name += &expected_extension;
        Ok(name)
    }

    /// Mimics `ValueToString::to_string`.
    pub fn value_to_string(&self) -> Vc<RcStr> {
        value_to_string(self.clone())
    }
}

#[turbo_tasks::function]

async fn value_to_string(ident: AssetIdent) -> Result<Vc<RcStr>> {
    let mut s = ident.path.value_to_string().owned().await?.into_owned();

    // The query string is either empty or non-empty starting with `?` so we can just concat
    s.push_str(&ident.query);
    // ditto for fragment
    s.push_str(&ident.fragment);

    if !ident.assets.is_empty() {
        s.push_str(" {");
        s.push_str(&ident.assets);
        s.push_str(" }");
    }

    if let Some(layer) = &ident.layer {
        s.push_str(" [");
        s.push_str(layer.name());
        s.push(']');
    }

    if !ident.modifiers.is_empty() {
        s.push_str(" (");
        s.push_str(&ident.modifiers);
        s.push(')');
    }

    if let Some(content_type) = &ident.content_type {
        write!(s, " <{content_type}>")?;
    }

    if !ident.parts.is_empty() {
        s.push_str(&ident.parts);
    }

    Ok(Vc::cell(s.into()))
}

fn clean_separators(s: &str) -> String {
    static SEPARATOR_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"[/#?]").unwrap());
    SEPARATOR_REGEX.replace_all(s, "_").to_string()
}

fn clean_additional_extensions(s: &str) -> String {
    s.replace('.', "_")
}

// Re-export the macro in this module's namespace
pub use crate::new_layer;
