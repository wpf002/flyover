// TODO(M5): mount the wasm build of crates/flyover-render on a full-screen canvas and point it
// at `${NEXT_PUBLIC_API_URL}/tilesets/${tileSetId}`.
// Needs: wasm-pack output copied into apps/web/public/renderer, a WebGPU feature check with a
// plain "your browser doesn't support WebGPU" fallback, and the layer picker UI (M8).

export default async function ViewPage({ params }: { params: Promise<{ tileSetId: string }> }) {
  const { tileSetId } = await params;

  return (
    <main className="shell viewer-wrap">
      <p className="back">
        <a href="/">← Back to repositories</a>
      </p>
      <div className="section-head">
        <h2>Viewer</h2>
        <span className="count">M3–M5</span>
      </div>
      <div className="state">
        <h3>The renderer isn’t built yet</h3>
        <p>
          There’s nothing to show for tile set <code>{tileSetId}</code> until the wgpu renderer
          ships. Follow the milestones in <code>docs/SPEC.md</code> (M3 native, M5 web).
        </p>
      </div>
    </main>
  );
}
