import type { RepoDto } from "@flyover/types";
import { SubmitRepo } from "./submit-repo";

// Reads live data from the API on every request. Never prerender at build time.
export const dynamic = "force-dynamic";

const API_URL = process.env.API_URL ?? "http://localhost:4000";

async function getRepos(): Promise<{ repos: RepoDto[] } | { error: string }> {
  try {
    const res = await fetch(`${API_URL}/repos`, { cache: "no-store" });
    if (!res.ok) return { error: `The API answered ${res.status}` };
    return { repos: (await res.json()) as RepoDto[] };
  } catch {
    return { error: `Can't reach the API at ${API_URL}` };
  }
}

const dateFmt = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "short",
  day: "numeric",
  timeZone: "UTC",
});

function Mark() {
  return (
    <svg viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M3 8.5 12 3l9 5.5-9 5.5-9-5.5Z"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
      <path
        d="M3 12.5 12 18l9-5.5"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
        opacity="0.65"
      />
      <path
        d="M3 16.5 12 22l9-5.5"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
        opacity="0.35"
      />
    </svg>
  );
}

function Tick() {
  return (
    <svg className="tick" viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <circle cx="10" cy="10" r="8.25" stroke="currentColor" strokeWidth="1.5" />
      <path
        d="m6.5 10.2 2.3 2.3 4.7-4.9"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

const STAGES = [
  { key: "scan", name: "Scan", sub: "files, lines, languages", cls: "done" },
  { key: "index", name: "Index", sub: "symbols + imports · M1", cls: "done" },
  { key: "layout", name: "Layout + tiles", sub: "treemap, quadtree · M2", cls: "next" },
  { key: "render", name: "Renderer", sub: "wgpu flythrough · M3–M5", cls: "later" },
];

export default async function HomePage() {
  const result = await getRepos();
  const repos = "repos" in result ? result.repos : [];
  const error = "error" in result ? result.error : null;

  return (
    <>
      <header className="shell">
        <div className="topbar">
          <span className="brand">
            <Mark />
            <span className="brand-name">Flyover</span>
          </span>
          <nav className="topnav">
            <a href="https://github.com/wpf002/flyover" target="_blank" rel="noreferrer">
              GitHub
            </a>
            <a
              href="https://github.com/wpf002/flyover/blob/main/docs/SPEC.md"
              target="_blank"
              rel="noreferrer"
            >
              Spec
            </a>
          </nav>
        </div>
      </header>

      <main className="shell">
        <section className="hero">
          <span className="eyebrow">
            <span className="dot" />
            Indexer live · renderer in progress
          </span>
          <h1>
            Fly through <span className="grad">any codebase</span> in 3D.
          </h1>
          <p className="lede">
            Directories become districts, files become blocks sized by line count, and the source is
            readable up close. The renderer only ever streams quadtree tiles, so a 5K-line repo and
            Chromium (~51M lines) render the same way.
          </p>
          <div className="hero-meta">
            <span>
              <b>10</b> languages indexed
            </span>
            <span>
              <b>4.4M</b> lines in ~1s
            </span>
            <span>Deterministic, tile-streamed</span>
          </div>
        </section>

        <section className="panel submit-panel" aria-labelledby="add-heading">
          <h2 id="add-heading">Add a repository</h2>
          <p className="hint">
            Point Flyover at any public git repo — paste an https URL, even Chromium. Adding it
            registers the repo now; the index → tiles → flythrough pipeline is being built (M2–M5).
          </p>
          <SubmitRepo />
        </section>

        <div className="section-head">
          <h2>Repositories</h2>
          <span className="count">
            {repos.length} {repos.length === 1 ? "repo" : "repos"}
          </span>
        </div>

        {error ? (
          <div className="state error">
            <h3>Can’t reach the API</h3>
            <p>
              {error}. Start the services with <code>pnpm dev</code>, or check that the API is on{" "}
              <code>{API_URL}</code>.
            </p>
          </div>
        ) : repos.length === 0 ? (
          <div className="state">
            <h3>No repositories yet</h3>
            <p>
              Add one above to get started — try the Chromium chip, or paste any git URL you want to
              map.
            </p>
          </div>
        ) : (
          <div className="repo-grid">
            {repos.map((repo) => (
              <article key={repo.id} className="repo-card">
                <div className="repo-top">
                  <span className="repo-name" title={repo.name}>
                    {repo.name}
                  </span>
                  <span className="pill">
                    <span className="pip" />
                    Registered
                  </span>
                </div>
                <span className="repo-source">{repo.source}</span>
                <div className="repo-foot">
                  <span>Added {dateFmt.format(new Date(repo.createdAt))}</span>
                  <span>index → M2</span>
                </div>
              </article>
            ))}
          </div>
        )}

        <div className="section-head">
          <h2>Pipeline</h2>
          <span className="count">acquire → index → layout → tile → render</span>
        </div>
        <section className="pipeline">
          {STAGES.map((stage) => (
            <div key={stage.key} className={`stage ${stage.cls}`}>
              <div className="stage-top">
                <Tick />
                {stage.name}
              </div>
              <div className="stage-sub">{stage.sub}</div>
            </div>
          ))}
        </section>

        <footer className="footer">
          <span>
            Index a repo today: <code>flyover index &lt;path&gt; -o &lt;dir&gt;</code>
          </span>
          <span>Built with Rust · wgpu · Next.js</span>
        </footer>
      </main>
    </>
  );
}
