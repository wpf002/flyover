-- CreateEnum
CREATE TYPE "JobStatus" AS ENUM ('QUEUED', 'RUNNING', 'SUCCEEDED', 'FAILED');

-- CreateEnum
CREATE TYPE "ShapeSource" AS ENUM ('GENERATED', 'UPLOADED', 'RECTANGLE');

-- CreateTable
CREATE TABLE "Repo" (
    "id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "source" TEXT NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updatedAt" TIMESTAMP(3) NOT NULL,

    CONSTRAINT "Repo_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "IndexJob" (
    "id" TEXT NOT NULL,
    "repoId" TEXT NOT NULL,
    "status" "JobStatus" NOT NULL DEFAULT 'QUEUED',
    "ref" TEXT,
    "commitSha" TEXT,
    "error" TEXT,
    "stats" JSONB,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "startedAt" TIMESTAMP(3),
    "finishedAt" TIMESTAMP(3),

    CONSTRAINT "IndexJob_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "TileSet" (
    "id" TEXT NOT NULL,
    "repoId" TEXT NOT NULL,
    "jobId" TEXT NOT NULL,
    "commitSha" TEXT NOT NULL,
    "formatVersion" INTEGER NOT NULL,
    "storagePrefix" TEXT NOT NULL,
    "shapeSource" "ShapeSource" NOT NULL DEFAULT 'GENERATED',
    "fileCount" INTEGER NOT NULL,
    "lineCount" BIGINT NOT NULL,
    "maxZoom" INTEGER NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "TileSet_pkey" PRIMARY KEY ("id")
);

-- CreateTable
CREATE TABLE "Layer" (
    "id" TEXT NOT NULL,
    "tileSetId" TEXT NOT NULL,
    "key" TEXT NOT NULL,
    "label" TEXT NOT NULL,
    "kind" TEXT NOT NULL,
    "meta" JSONB NOT NULL,
    "createdAt" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT "Layer_pkey" PRIMARY KEY ("id")
);

-- CreateIndex
CREATE UNIQUE INDEX "Repo_source_key" ON "Repo"("source");

-- CreateIndex
CREATE INDEX "IndexJob_status_createdAt_idx" ON "IndexJob"("status", "createdAt");

-- CreateIndex
CREATE INDEX "IndexJob_repoId_idx" ON "IndexJob"("repoId");

-- CreateIndex
CREATE UNIQUE INDEX "TileSet_jobId_key" ON "TileSet"("jobId");

-- CreateIndex
CREATE UNIQUE INDEX "TileSet_storagePrefix_key" ON "TileSet"("storagePrefix");

-- CreateIndex
CREATE INDEX "TileSet_repoId_createdAt_idx" ON "TileSet"("repoId", "createdAt");

-- CreateIndex
CREATE UNIQUE INDEX "Layer_tileSetId_key_key" ON "Layer"("tileSetId", "key");

-- AddForeignKey
ALTER TABLE "IndexJob" ADD CONSTRAINT "IndexJob_repoId_fkey" FOREIGN KEY ("repoId") REFERENCES "Repo"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "TileSet" ADD CONSTRAINT "TileSet_repoId_fkey" FOREIGN KEY ("repoId") REFERENCES "Repo"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "TileSet" ADD CONSTRAINT "TileSet_jobId_fkey" FOREIGN KEY ("jobId") REFERENCES "IndexJob"("id") ON DELETE CASCADE ON UPDATE CASCADE;

-- AddForeignKey
ALTER TABLE "Layer" ADD CONSTRAINT "Layer_tileSetId_fkey" FOREIGN KEY ("tileSetId") REFERENCES "TileSet"("id") ON DELETE CASCADE ON UPDATE CASCADE;
