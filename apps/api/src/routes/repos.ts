import { getPrisma, type Repo } from "@flyover/db";
import type { ApiError, RepoDto } from "@flyover/types";
import type { FastifyInstance } from "fastify";
import { z } from "zod";

const createRepoBody = z.object({
  // https only. Host allowlisting and SSRF checks happen in the worker before clone
  // (docs/SPEC.md, Security). This check just keeps obvious junk out of the table.
  source: z
    .url()
    .refine((u) => new URL(u).protocol === "https:", "source must be an https git URL"),
  name: z.string().min(1).max(200).optional(),
});

function toDto(repo: Repo): RepoDto {
  return {
    id: repo.id,
    name: repo.name,
    source: repo.source,
    createdAt: repo.createdAt.toISOString(),
  };
}

function nameFromSource(source: string): string {
  const last = new URL(source).pathname.split("/").filter(Boolean).pop() ?? source;
  return last.replace(/\.git$/, "");
}

export async function repoRoutes(app: FastifyInstance): Promise<void> {
  app.get("/repos", async (): Promise<RepoDto[]> => {
    const repos = await getPrisma().repo.findMany({ orderBy: { createdAt: "desc" } });
    return repos.map(toDto);
  });

  app.post("/repos", async (req, reply): Promise<RepoDto | ApiError> => {
    const parsed = createRepoBody.safeParse(req.body);
    if (!parsed.success) {
      reply.code(400);
      return { error: "bad_request", message: z.prettifyError(parsed.error) };
    }
    const { source, name } = parsed.data;
    const repo = await getPrisma().repo.upsert({
      where: { source },
      update: {},
      create: { source, name: name ?? nameFromSource(source) },
    });
    reply.code(201);
    return toDto(repo);
  });

  app.get<{ Params: { id: string } }>("/repos/:id", async (req, reply) => {
    const repo = await getPrisma().repo.findUnique({ where: { id: req.params.id } });
    if (!repo) {
      reply.code(404);
      return { error: "not_found", message: "repo not found" } satisfies ApiError;
    }
    return toDto(repo);
  });
}
