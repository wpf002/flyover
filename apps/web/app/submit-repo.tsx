"use client";

import { useActionState, useState } from "react";
import { addRepo, type AddRepoState } from "./actions";

const EXAMPLES = [
  { label: "Chromium", url: "https://github.com/chromium/chromium.git" },
  { label: "React", url: "https://github.com/facebook/react.git" },
  { label: "Flyover", url: "https://github.com/wpf002/flyover.git" },
];

export function SubmitRepo() {
  const [state, action, pending] = useActionState<AddRepoState, FormData>(addRepo, {});
  const [value, setValue] = useState("");

  return (
    <form action={action}>
      <div className="submit-row">
        <input
          name="source"
          type="url"
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          placeholder="https://github.com/chromium/chromium.git"
          aria-label="Git repository URL"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
        <button type="submit" className="btn btn-primary" disabled={pending}>
          {pending ? (
            <>
              <span className="spinner" aria-hidden="true" /> Adding…
            </>
          ) : (
            "Add repository"
          )}
        </button>
      </div>

      <div className="chips">
        <span className="chips-label">Try</span>
        {EXAMPLES.map((ex) => (
          <button key={ex.url} type="button" className="chip" onClick={() => setValue(ex.url)}>
            {ex.label}
          </button>
        ))}
      </div>

      {state.error ? (
        <p className="form-msg error" role="alert">
          {state.error}
        </p>
      ) : state.ok ? (
        <p className="form-msg ok" role="status">
          Added <strong>{state.name}</strong> and queued it for indexing. Progress shows below; a
          Fly link appears when its map is ready.
        </p>
      ) : null}
    </form>
  );
}
