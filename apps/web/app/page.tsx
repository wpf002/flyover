import type { IndexJobDto, JobStage, RepoDto } from "@flyover/types";

import { queueJob } from "./actions";
import { LiveRefresh } from "./live-refresh";
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
const numFmt = new Intl.NumberFormat("en-US");

const STAGE_LABEL: Record<JobStage, string> = {
  cloning: "Cloning",
  indexing: "Indexing",
  layout: "Laying out",
  uploading: "Uploading",
  done: "Finishing",
};

function isActive(job: IndexJobDto | null): boolean {
  return job?.status === "QUEUED" || job?.status === "RUNNING";
}

function Status({ job }: { job: IndexJobDto | null }) {
  if (!job) return <Pill tone="idle">Not indexed</Pill>;
  switch (job.status) {
    case "QUEUED":
      return <Pill tone="wait">Queued</Pill>;
    case "RUNNING":
      return (
        <Pill tone="busy">{job.stats?.stage ? STAGE_LABEL[job.stats.stage] : "Starting"}</Pill>
      );
    case "SUCCEEDED":
      return <Pill tone="ok">Ready</Pill>;
    case "FAILED":
      return <Pill tone="bad">Failed</Pill>;
  }
}

function Pill({ tone, children }: { tone: string; children: React.ReactNode }) {
  return (
    <span className={`pill pill-${tone}`}>
      <span className="pip" />
      {children}
    </span>
  );
}

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
  { key: "index", name: "Index", sub: "symbols + imports · M1", cls: "done" },
  { key: "layout", name: "Layout + tiles", sub: "treemap, quadtree · M2", cls: "done" },
  { key: "render", name: "Native renderer", sub: "wgpu flythrough · M3", cls: "done" },
  { key: "web", name: "Web pipeline", sub: "worker, jobs, browser viewer · M5", cls: "next" },
];

function RepoCard({ repo }: { repo: RepoDto }) {
  const job = repo.latestJob;
  const tiles = repo.latestTileSet;
  const active = isActive(job);
  return (
    <article className="repo-card">
      <div className="repo-top">
        <span className="repo-name" title={repo.name}>
          {repo.name}
        </span>
        <Status job={job} />
      </div>
      <span className="repo-source">{repo.source}</span>
      {tiles ? (
        <span className="repo-stats">
          {numFmt.format(tiles.fileCount)} files · {numFmt.format(tiles.lineCount)} lines
        </span>
      ) : null}
      {job?.status === "FAILED" && job.error ? (
        <span className="repo-error" title={job.error}>
          {job.error}
        </span>
      ) : null}
      <div className="repo-foot">
        <span>Added {dateFmt.format(new Date(repo.createdAt))}</span>
        {tiles && !active ? (
          <a className="fly-link" href={`/view/${tiles.id}`}>
            Fly →
          </a>
        ) : active ? (
          <span className="working">working…</span>
        ) : (
          <form action={queueJob}>
            <input type="hidden" name="repoId" value={repo.id} />
            <button type="submit" className="link-btn">
              {job?.status === "FAILED" ? "Retry" : "Index"}
            </button>
          </form>
        )}
      </div>
    </article>
  );
}

export default async function HomePage() {
  const result = await getRepos();
  const repos = "repos" in result ? result.repos : [];
  const error = "error" in result ? result.error : null;
  const anyActive = repos.some((r) => isActive(r.latestJob));

  return (
    <>
      <LiveRefresh active={anyActive} />
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
            Index, tiles, and native renderer live
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
              <b>4.4M</b> lines indexed in ~1s
            </span>
            <span>
              <b>0.5 ms</b> frames at 1440p
            </span>
          </div>
        </section>

        <section className="panel submit-panel" aria-labelledby="add-heading">
          <h2 id="add-heading">Add a repository</h2>
          <p className="hint">
            Paste any public https git URL on GitHub, GitLab, or Bitbucket, even Chromium. Flyover
            clones it (shallow, hooks off), indexes it, and cuts it into map tiles.
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
              Add one above to get started. Try the Chromium chip, or paste any git URL you want to
              map.
            </p>
          </div>
        ) : (
          <div className="repo-grid">
            {repos.map((repo) => (
              <RepoCard key={repo.id} repo={repo} />
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
            Fly a tile set natively: <code>flyover view &lt;tileset-dir&gt;</code>
          </span>
          <span>Built with Rust · wgpu · Next.js</span>
        </footer>
      </main>
    </>
  );
}
