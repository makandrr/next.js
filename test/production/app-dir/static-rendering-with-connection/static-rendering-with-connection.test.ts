import { nextTestSetup } from 'e2e-utils'

describe('static-rendering-with-connection', () => {
  const { next, isNextDeploy } = nextTestSetup({
    files: __dirname,
    skipStart: true,
    skipDeployment: true,
    env: {
      __NEXT_PRIVATE_DETERMINISTIC_BUILD_OUTPUT: '1',
    },
  })

  if (isNextDeploy) {
    it.skip('should skip deployment', () => {})
    return
  }

  beforeAll(async () => {
    await next.build()
  })

  if (process.env.__NEXT_EXPERIMENTAL_CACHE_COMPONENTS === 'true') {
    it('should mark routes with connection() as partial prerendered', async () => {
      // When cache components are enabled, routes with connection() should be
      // marked as partial prerendered.
      expect(getTreeView(next.cliOutput)).toMatchInlineSnapshot(`
       "Route (app)
       ┌ ○ /_not-found
       └ ◐ /blog/[slug]
           ├ /blog/[slug]
           ├ /blog/slug-01
           ├ /blog/slug-02
           └ /blog/slug-03


       ○  (Static)             prerendered as static content
       ◐  (Partial Prerender)  prerendered as static HTML with dynamic server-streamed content"
      `)
    })
  } else {
    it('should mark routes with connection() as dynamic, not SSG', async () => {
      // Routes with generateStaticParams that use connection() should be marked as dynamic
      // because connection() makes the page dynamic. This is a regression test for a bug
      // where pages with generateStaticParams were incorrectly marked as SSG even when
      // the individual routes used dynamic APIs.
      expect(getTreeView(next.cliOutput)).toMatchInlineSnapshot(`
            "Route (app)
            ┌ ○ /_not-found
            └ ƒ /blog/[slug]
                ├ /blog/slug-01
                ├ /blog/slug-02
                └ /blog/slug-03


            ○  (Static)   prerendered as static content
            ƒ  (Dynamic)  server-rendered on demand"
          `)
    })
  }

  it('should render the blog pages correctly', async () => {
    await next.start()

    // Test one of the generated routes
    const $ = await next.render$('/blog/slug-01')
    expect($('#page').text()).toBe('Page slug-01')

    await next.stop()
  })
})

/**
 * Extracts the route tree view from the build output.
 * This captures everything from "Route " onwards to show the build output.
 */
function getTreeView(cliOutput: string): string {
  let foundStart = false
  const lines: string[] = []

  for (const line of cliOutput.split('\n')) {
    foundStart ||= line.startsWith('Route ')

    if (foundStart) {
      lines.push(line)
    }
  }

  return lines.join('\n').trim()
}
