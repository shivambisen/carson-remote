import { useEffect, useState } from "react";
import { Apple, Download, Laptop, MoreHorizontal, MonitorSmartphone, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { api, type Device } from "@/api";
import { osLabel, timeAgo } from "@/format";
import { keyId } from "@/identity";
import { errorMessage } from "@/lib/errors";
import { DOWNLOADS_URL } from "@/lib/links";
import { toLogin } from "@/lib/router";
import { cn } from "@/lib/utils";
import { PresenceClient, type ConnectionStatus } from "@/signaling";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

function OsIcon({ os, className }: { os: string; className?: string }) {
  return os === "macos" ? <Apple className={className} /> : <Laptop className={className} />;
}

function DeviceRow({ device, onRemove }: { device: Device; onRemove: (d: Device) => void }) {
  return (
    <div className="flex items-center gap-3 px-4 py-4 sm:gap-4 sm:px-5">
      <div className="relative shrink-0">
        <div className="grid size-11 place-items-center rounded-lg bg-muted text-muted-foreground">
          <OsIcon os={device.os} className="size-5" />
        </div>
        <span
          className={cn("absolute -right-0.5 -bottom-0.5 size-3 rounded-full border-2 border-card", device.online ? "bg-success" : "bg-muted-foreground/40")}
          aria-label={device.online ? "Online" : "Offline"}
        />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{device.name}</span>
          {device.online ? (
            <Badge variant="success" className="hidden sm:inline-flex">
              Online
            </Badge>
          ) : (
            <Badge variant="secondary" className="hidden sm:inline-flex">
              Offline
            </Badge>
          )}
        </div>
        <div className="mt-0.5 truncate text-sm text-muted-foreground">
          <span className="sm:hidden">{device.online ? "Online · " : "Offline · "}</span>
          {osLabel(device.os)}
          {!device.online && ` · last seen ${timeAgo(device.last_seen_at)}`}
          {device.public_key && (
            <Tooltip>
              <TooltipTrigger asChild>
                <span className="ml-2 hidden font-mono text-xs sm:inline">{keyId(device.public_key)}</span>
              </TooltipTrigger>
              <TooltipContent>Device key. The carson app on that computer shows the same value.</TooltipContent>
            </Tooltip>
          )}
        </div>
      </div>
      <Button disabled={!device.online} asChild={device.online}>
        {device.online ? <a href={`#/view/${encodeURIComponent(device.id)}`}>Connect</a> : <span>Connect</span>}
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" aria-label="More">
            <MoreHorizontal />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem variant="destructive" onSelect={() => onRemove(device)}>
            <Trash2 /> Remove from account
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function EmptyState() {
  return (
    <div className="flex flex-col items-center gap-4 px-6 py-14 text-center">
      <div className="grid size-12 place-items-center rounded-full bg-accent text-accent-foreground">
        <MonitorSmartphone className="size-6" />
      </div>
      <div className="space-y-1">
        <h2 className="font-semibold">Add your first computer</h2>
        <p className="max-w-sm text-sm text-muted-foreground">
          Install the carson app on the computer you want to reach and sign in with this same account. It shows up here on its own.
        </p>
      </div>
      <Button asChild>
        <a href={DOWNLOADS_URL} target="_blank" rel="noreferrer">
          <Download /> Download carson
        </a>
      </Button>
      <a href="#/pair" className="text-xs text-muted-foreground underline-offset-4 hover:underline">
        Have a code instead?
      </a>
    </div>
  );
}

export function DevicesPage() {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [removing, setRemoving] = useState<Device | null>(null);

  useEffect(() => {
    const client = new PresenceClient((s) => setDevices(s.devices), setStatus, toLogin);
    client.start();
    return () => client.stop();
  }, []);

  const remove = (d: Device) =>
    api.revokeDevice(d.id).then(
      () => {
        setDevices((list) => list?.filter((x) => x.id !== d.id) ?? null);
        toast.success(`Removed ${d.name}`);
      },
      (e) => toast.error(errorMessage(e)),
    );

  const sorted = devices && [...devices].sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name));

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="font-serif text-4xl tracking-[-0.02em]">Devices</h1>
          <p className="text-sm text-muted-foreground">Computers on your account. Click Connect to see and control one.</p>
        </div>
        <div className="flex items-center gap-3">
          {status !== "live" && <span className="text-sm text-muted-foreground">{status === "connecting" ? "Connecting…" : "Reconnecting…"}</span>}
          {sorted && sorted.length > 0 && (
            <Button variant="outline" asChild>
              <a href={DOWNLOADS_URL} target="_blank" rel="noreferrer">
                <Download /> Add a computer
              </a>
            </Button>
          )}
        </div>
      </div>
      <Card className="gap-0 py-0">
        <CardContent className="p-0">
          {sorted === null ? (
            <div className="space-y-px">
              {[0, 1].map((i) => (
                <div key={i} className="flex items-center gap-4 px-5 py-4">
                  <div className="size-11 animate-pulse rounded-lg bg-muted" />
                  <div className="flex-1 space-y-2">
                    <div className="h-4 w-40 animate-pulse rounded bg-muted" />
                    <div className="h-3 w-24 animate-pulse rounded bg-muted" />
                  </div>
                </div>
              ))}
            </div>
          ) : sorted.length === 0 ? (
            <EmptyState />
          ) : (
            <div className="divide-y">
              {sorted.map((d) => (
                <DeviceRow key={d.id} device={d} onRemove={setRemoving} />
              ))}
            </div>
          )}
        </CardContent>
      </Card>
      <AlertDialog open={removing !== null} onOpenChange={(open) => !open && setRemoving(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Remove {removing?.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              It's disconnected right away and can't be reached any more. To use it again, sign in on that computer again.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => removing && void remove(removing)}>
              Remove
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
