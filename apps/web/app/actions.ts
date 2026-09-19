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
 * Register a repository through the real POST /repos endpoint (an M0 route that upserts a Repo
 * row). This does not index or build tiles yet — that pipeline lands in M2+ / the worker in M5.
 * Runs on the server, so it reaches the API without a CORS preflight.
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
      const repo = (await res.json()) as { name: string };
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
