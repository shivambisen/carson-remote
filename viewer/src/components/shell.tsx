import type { ReactNode } from "react";
import { Activity, LogOut } from "lucide-react";

import type { User } from "@/api";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { MARK_PATH } from "@/lib/logo";
import { cn } from "@/lib/utils";

/** The glenley mark in the current text colour. */
export function Mark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 100 100" className={className} aria-hidden="true">
      <path fill="currentColor" fillRule="evenodd" d={MARK_PATH} />
    </svg>
  );
}

export function Logo({ className }: { className?: string }) {
  return (
    <a href="#/devices" className={cn("flex items-center gap-2 font-semibold tracking-tight", className)}>
      <Mark className="size-7" />
      glenley
    </a>
  );
}

function initials(user: User): string {
  const name = user.display_name || user.email;
  return name
    .split(/[\s@.]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((p) => p[0]?.toUpperCase())
    .join("");
}

export function Shell({ user, onSignOut, active, children }: { user: User | null; onSignOut: () => void; active?: "devices" | "activity"; children: ReactNode }) {
  const nav = (href: string, label: string, key: "devices" | "activity") => (
    <a
      href={href}
      className={cn("rounded-md px-3 py-1.5 text-sm transition-colors hover:text-foreground", active === key ? "bg-secondary font-medium text-foreground" : "text-muted-foreground")}
    >
      {label}
    </a>
  );
  return (
    <div className="min-h-svh">
      <header className="sticky top-0 z-40 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-5xl items-center gap-6 px-4">
          <Logo />
          {user && (
            <nav className="flex items-center gap-1">
              {nav("#/devices", "Devices", "devices")}
              {nav("#/activity", "Activity", "activity")}
            </nav>
          )}
          <div className="flex-1" />
          {user && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="rounded-full" aria-label="Account">
                  <span className="grid size-8 place-items-center rounded-full bg-accent text-xs font-semibold text-accent-foreground">{initials(user)}</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-56">
                <DropdownMenuLabel className="font-normal">
                  <div className="truncate text-sm font-medium">{user.display_name || user.email}</div>
                  <div className="truncate text-xs text-muted-foreground">{user.email}</div>
                </DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => (location.hash = "#/activity")}>
                  <Activity /> Activity
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={onSignOut}>
                  <LogOut /> Sign out
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      </header>
      <main className="mx-auto max-w-5xl px-4 py-8">{children}</main>
    </div>
  );
}

/** A centered, narrow layout for sign-in and one-off pages. */
export function Centered({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-8 bg-[radial-gradient(circle,color-mix(in_oklab,var(--foreground)_12%,transparent)_1px,transparent_1.3px)] bg-[length:22px_22px] px-4 py-10">
      <Logo className="text-lg" />
      <div className="w-full max-w-sm">{children}</div>
    </div>
  );
}
