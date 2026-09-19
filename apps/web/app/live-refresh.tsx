"use client";

import { useRouter } from "next/navigation";
import { useEffect } from "react";

/** Re-render the page every couple of seconds while any index job is still running. */
export function LiveRefresh({ active }: { active: boolean }) {
  const router = useRouter();
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => router.refresh(), 2000);
    return () => clearInterval(timer);
  }, [active, router]);
  return null;
}
