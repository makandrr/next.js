import { connection } from 'next/server'

export async function generateStaticParams() {
  return [{ slug: 'slug-01' }, { slug: 'slug-02' }, { slug: 'slug-03' }]
}

export default async function Page({
  params,
}: {
  params: Promise<{ slug: string }>
}) {
  await connection()
  const { slug } = await params

  return <div id="page">Page {slug}</div>
}
