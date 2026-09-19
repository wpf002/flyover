import type { TileSetDto, TileSetManifest } from "@flyover/types";

// TODO(M5): mount the wasm build of crates/flyover-render on the canvas below and point it at
// `${NEXT_PUBLIC_API_URL}/tilesets/${tileSetId}/files/`. Needs the wasm32 + WebGPU build of the
// renderer (HTTP tile fetch instead of disk, decode in web workers) and a WebGPU feature check.

export const dynamic = "force-dynamic";

const API_URL = process.env.API_URL ?? "http://localhost:4000";
const numFmt = new Intl.NumberFormat("en-US");

async function load(id: string): Promise<{ set: TileSetDto; manifest: TileSetManifest } | null> {
  try {
    const [setRes, manifestRes] = await Promise.all([
      fetch(`${API_URL}/tilesets/${encodeURIComponent(id)}`, { cache: "no-store" }),
      fetch(`${API_URL}/tilesets/${encodeURIComponent(id)}/manifest`, { cache: "no-store" }),
    ]);
    if (!setRes.ok || !manifestRes.ok) return null;
    return {
      set: (await setRes.json()) as TileSetDto,
      manifest: (await manifestRes.json()) as TileSetManifest,
    };
  } catch {
    return null;
  }
}

export default async function ViewPage({ params }: { params: Promise<{ tileSetId: string }> }) {
  const { tileSetId } = await params;
  const data = await load(tileSetId);

  if (!data) {
    return (
      <main className="shell viewer-wrap">
        <p className="back">
          <a href="/">← Back to repositories</a>
        </p>
        <div className="state error">
          <h3>Tile set not found</h3>
          <p>
            There is no tile set <code>{tileSetId}</code>, or the API can’t reach its storage.
          </p>
        </div>
      </main>
    );
  }

  const { set, manifest } = data;
  const languages = manifest.layers.find((l) => l.key === "language")?.categories ?? [];

  return (
    <main className="shell viewer-wrap">
      <p className="back">
        <a href="/">← Back to repositories</a>
      </p>
      <div className="section-head">
        <h2>{manifest.repo.name}</h2>
        <span className="count" title={manifest.repo.commitSha}>
          {manifest.repo.commitSha.slice(0, 10)}
        </span>
      </div>

      <div className="viewer-stats">
        <span>
          <b>{numFmt.format(manifest.stats.files)}</b> files
        </span>
        <span>
          <b>{numFmt.format(manifest.stats.directories)}</b> directories
        </span>
        <span>
          <b>{numFmt.format(manifest.stats.lines)}</b> lines
        </span>
        <span>
          <b>{manifest.stats.languages}</b> languages
        </span>
        <span>
          zoom <b>0–{manifest.maxZoom}</b>
        </span>
      </div>

      <div className="viewer-stage" id="flyover-canvas-mount" data-tileset={set.id}>
        <div className="state">
          <h3>Browser renderer coming next</h3>
          <p>
            The map is built and served by the API. To fly it now, run the native viewer on the same
            tile set: <code>flyover view &lt;TILE_STORAGE_DIR&gt;/tilesets/&lt;job&gt;</code>.
          </p>
        </div>
      </div>

      {languages.length > 0 ? (
        <div className="legend" aria-label="Language colors">
          {languages.map((c) => (
            <span key={c.label} className="legend-item">
              <span className="swatch" style={{ background: c.color }} />
              {c.label}
            </span>
          ))}
        </div>
      ) : null}
    </main>
  );
}
