import {
  GetObjectCommand,
  HeadObjectCommand,
  PutObjectCommand,
  S3Client,
  S3ServiceException,
} from "@aws-sdk/client-s3";
import type { Readable } from "node:stream";

import {
  contentTypeFor,
  IMMUTABLE,
  safeRelativePath,
  type StoredObject,
  type TileStorage,
} from "./core.js";

export interface S3Config {
  /** Custom endpoint for S3-compatible providers (R2, MinIO, ...). Omit for AWS. */
  endpoint: string | undefined;
  region: string;
  bucket: string;
  accessKeyId: string;
  secretAccessKey: string;
}

function isMissing(err: unknown): boolean {
  return (
    err instanceof S3ServiceException &&
    (err.name === "NoSuchKey" || err.name === "NotFound" || err.$metadata.httpStatusCode === 404)
  );
}

/** Any S3-compatible bucket. Keys are validated the same way as the fs driver. */
export class S3Storage implements TileStorage {
  readonly driver = "s3" as const;

  constructor(
    private readonly client: S3Client,
    private readonly bucket: string,
  ) {}

  static fromConfig(config: S3Config): S3Storage {
    const client = new S3Client({
      region: config.region,
      ...(config.endpoint ? { endpoint: config.endpoint, forcePathStyle: true } : {}),
      credentials: {
        accessKeyId: config.accessKeyId,
        secretAccessKey: config.secretAccessKey,
      },
    });
    return new S3Storage(client, config.bucket);
  }

  async put(key: string, body: Uint8Array, contentType?: string): Promise<void> {
    if (safeRelativePath(key) === null) {
      throw new Error(`refusing unsafe storage key: ${JSON.stringify(key)}`);
    }
    await this.client.send(
      new PutObjectCommand({
        Bucket: this.bucket,
        Key: key,
        Body: body,
        ContentType: contentType ?? contentTypeFor(key),
        CacheControl: IMMUTABLE,
      }),
    );
  }

  async get(key: string): Promise<StoredObject | null> {
    if (safeRelativePath(key) === null) return null;
    try {
      const out = await this.client.send(new GetObjectCommand({ Bucket: this.bucket, Key: key }));
      if (!out.Body) return null;
      return {
        body: out.Body as Readable,
        size: out.ContentLength,
        contentType: out.ContentType ?? contentTypeFor(key),
      };
    } catch (err) {
      if (isMissing(err)) return null;
      throw err;
    }
  }

  async exists(key: string): Promise<boolean> {
    if (safeRelativePath(key) === null) return false;
    try {
      await this.client.send(new HeadObjectCommand({ Bucket: this.bucket, Key: key }));
      return true;
    } catch (err) {
      if (isMissing(err)) return false;
      throw err;
    }
  }
}
