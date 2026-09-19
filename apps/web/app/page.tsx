import type { RepoDto } from "@flyover/types";

// Reads live data from the API on every request. Never prerender at build time.
export const dynamic = "force-dynamic";

const API_URL = process.env.API_URL ?? "http://localhost:4000";

async function getRepos(): Promise<{ repos: RepoDto[] } | { error: string }> {
  try {
    const res = await fetch(`${API_URL}/repos`, { cache: "no-store" });
    if (!res.ok) return { error: `API answered ${res.status}` };
    return { repos: (await res.json()) as RepoDto[] };
  } catch {
    return { error: `Can't reach the API at ${API_URL}` };
  }
}

export default async function HomePage() {
  const result = await getRepos();

  return (
    <main>
      <h1>Flyover</h1>
      <p className="lede">Point it at a repo. Fly through the code.</p>

      {"error" in result ? (
        <div className="notice">{result.error}. Start it with `pnpm dev`.</div>
      ) : result.repos.length === 0 ? (
        <div className="notice">
          No repos yet. Add one with POST /repos. The submit form lands in M5.
        </div>
      ) : (
        <ul className="repos">
          {result.repos.map((repo) => (
            <li key={repo.id}>
              <span>{repo.name}</span>
              <span className="source">{repo.source}</span>
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
