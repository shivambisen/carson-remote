import { useEffect, useState } from "react";

import { api, type AuditEvent } from "@/api";
import { browserLabel } from "@/format";
import { errorMessage } from "@/lib/errors";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

const ACTIONS: Record<string, { label: string; tone: "default" | "secondary" | "destructive" | "success" | "outline" }> = {
  signed_in: { label: "Signed in", tone: "secondary" },
  device_paired: { label: "Computer added", tone: "success" },
  device_revoked: { label: "Computer removed", tone: "destructive" },
  session_requested: { label: "Connection requested", tone: "outline" },
  session_accepted: { label: "Connected", tone: "success" },
  session_rejected: { label: "Connection refused", tone: "destructive" },
  session_resumed: { label: "Reconnected", tone: "outline" },
  session_ended: { label: "Disconnected", tone: "secondary" },
};

export function ActivityPage() {
  const [events, setEvents] = useState<AuditEvent[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.audit().then(setEvents, (e) => setError(errorMessage(e)));
  }, []);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="font-serif text-4xl tracking-[-0.02em]">Activity</h1>
        <p className="text-sm text-muted-foreground">Sign-ins, computer changes and every remote connection on your account. Kept for 90 days.</p>
      </div>
      <Card className="py-0">
        <CardContent className="p-0">
          {error ? (
            <p className="p-6 text-sm text-destructive">{error}</p>
          ) : events === null ? (
            <p className="p-6 text-sm text-muted-foreground">Loading…</p>
          ) : events.length === 0 ? (
            <p className="p-6 text-sm text-muted-foreground">No activity yet.</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="pl-5">When</TableHead>
                  <TableHead>What</TableHead>
                  <TableHead>Computer</TableHead>
                  <TableHead>From</TableHead>
                  <TableHead>Details</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {events.map((e) => {
                  const a = ACTIONS[e.action] ?? { label: e.action, tone: "outline" as const };
                  return (
                    <TableRow key={e.id}>
                      <TableCell className="pl-5 text-muted-foreground" title={e.at}>
                        {new Date(e.at).toLocaleString()}
                      </TableCell>
                      <TableCell>
                        <Badge variant={a.tone}>{a.label}</Badge>
                      </TableCell>
                      <TableCell>{e.device_name ?? ""}</TableCell>
                      <TableCell className="text-muted-foreground">
                        <div>{e.ip ?? ""}</div>
                        {e.user_agent && <div className="text-xs">{browserLabel(e.user_agent)}</div>}
                      </TableCell>
                      <TableCell className="max-w-xs truncate text-muted-foreground" title={e.detail ?? ""}>
                        {e.detail ?? ""}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
