import { z } from "zod";

export interface User {
  id: string;
}

export type Id = string;

export enum Role {
  Admin,
  User,
}

export class Repo {
  find(): User | null {
    return null;
  }
}

export function make(): Repo {
  const _schema = z.string();
  return new Repo();
}
