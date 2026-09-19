// TODO(M5): mount the wasm build of crates/flyover-render on a full-screen canvas and point it
// at `${NEXT_PUBLIC_API_URL}/tilesets/${tileSetId}`.
// Needs: wasm-pack output copied into apps/web/public/renderer, WebGPU feature check with a
// plain "your browser doesn't support WebGPU" fallback, and the layer picker UI (M8).

export default async function ViewPage({ params }: { params: Promise<{ tileSetId: string }> }) {
  const { tileSetId } = await params;

  return (
    <main>
      <h1>Viewer</h1>
      <div className="notice">
        The renderer isn&apos;t built yet, so there&apos;s nothing to show for tile set{" "}
        <code>{tileSetId}</code>. See docs/SPEC.md, milestones M3 to M5.
      </div>
    </main>
  );
}
