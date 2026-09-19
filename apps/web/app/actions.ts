"use server";

import { revalidatePath } from "next/cache";

const API_URL = process.env.API_URL ?? "http://localhost:4000";

/** Result of an add-repo attempt. Kept structural so the client can mirror the shape. */
export type AddRepoState = {
  ok?: boolean;
  error?: string;
  name?: string;
};

/**
 * Register a repository and queue an index job for it. The worker (apps/worker) picks the job up,
 * clones the repo, and builds its tile set. Runs on the server, so it reaches the API without a
 * CORS preflight.
 */
export async function addRepo(_prev: AddRepoState, formData: FormData): Promise<AddRepoState> {
  const source = String(formData.get("source") ?? "").trim();
  if (!source) {
    return { error: "Enter a git URL to add." };
  }

  let parsed: URL;
  try {
    parsed = new URL(source);
  } catch {
    return { error: "That doesn't look like a URL." };
  }
  if (parsed.protocol !== "https:") {
    return { error: "The source must be an https git URL." };
  }

  try {
    const res = await fetch(`${API_URL}/repos`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source }),
      cache: "no-store",
    });

    if (res.status === 201) {
      const repo = (await res.json()) as { id: string; name: string };
      const job = await fetch(`${API_URL}/repos/${repo.id}/jobs`, {
        method: "POST",
        cache: "no-store",
      });
      if (!job.ok) {
        return { error: `Added ${repo.name}, but queueing its index job failed (${job.status}).` };
      }
      revalidatePath("/");
      return { ok: true, name: repo.name };
    }
    if (res.status === 400) {
      const body = (await res.json().catch(() => ({}))) as { message?: string };
      return { error: body.message ?? "The API rejected that URL." };
    }
    return { error: `The API answered ${res.status}.` };
  } catch {
    return { error: `Can't reach the API at ${API_URL}.` };
  }
}

/** Queue a fresh index job for an existing repo (re-index, or retry after a failure). */
export async function queueJob(formData: FormData): Promise<void> {
  const id = String(formData.get("repoId") ?? "");
  // Repo ids are cuids; anything else never reaches the API.
  if (!/^[a-z0-9]{8,40}$/i.test(id)) return;
  await fetch(`${API_URL}/repos/${id}/jobs`, { method: "POST", cache: "no-store" }).catch(
    () => undefined,
  );
  revalidatePath("/");
}
