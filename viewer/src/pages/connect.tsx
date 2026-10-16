import { useEffect, useRef, useState } from "react";
import { CheckCircle2, Loader2, XCircle } from "lucide-react";

import { api } from "@/api";
import { Centered } from "@/components/shell";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { errorMessage } from "@/lib/errors";

/** Validated loopback hand-off target: only 127.0.0.1 on a high port, with a hex state. */
export function loopbackUrl(params: URLSearchParams, grant: string): string | null {
  const port = Number(params.get("port"));
  const state = params.get("state") ?? "";
  if (!Number.isInteger(port) || port < 1024 || port > 65535 || !/^[0-9a-f]{16,64}$/.test(state)) return null;
  return `http://127.0.0.1:${port}/done?state=${state}&grant=${encodeURIComponent(grant)}`;
}

/**
 * Opened by the carson app (`#/connect?pairing=…&port=…&state=…`) after the user signs in:
 * gets a one-time grant and hands it to the app on this same computer.
 */
export function ConnectPage({ params }: { params: URLSearchParams }) {
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState<string | null>(null);
  const started = useRef(false);

  useEffect(() => {
    if (started.current) return; // StrictMode runs effects twice in development
    started.current = true;
    const pairing = params.get("pairing") ?? "";
    if (!/^pair_[A-Za-z0-9]+$/.test(pairing) || !loopbackUrl(params, "x")) {
      setError("This link is incomplete. Open carson on your computer and choose Sign in again.");
      return;
    }
    api.pairGrant(pairing).then(
      (g) => {
        setName(g.name);
        location.href = loopbackUrl(params, g.grant)!;
      },
      (e) => setError(errorMessage(e)),
    );
  }, [params]);

  return (
    <Centered>
      <Card>
        <CardHeader className="items-center text-center">
          {error ? <XCircle className="mx-auto size-10 text-destructive" /> : <Loader2 className="mx-auto size-10 animate-spin text-primary" />}
          <CardTitle className="font-serif text-3xl font-normal tracking-[-0.02em]">{error ? "Couldn't add this computer" : name ? `Adding ${name}…` : "Adding this computer…"}</CardTitle>
          <CardDescription>{error ?? "Finishing sign-in in the carson app."}</CardDescription>
        </CardHeader>
        {error && (
          <CardContent>
            <Button className="w-full" variant="outline" asChild>
              <a href="#/devices">Go to my devices</a>
            </Button>
          </CardContent>
        )}
      </Card>
    </Centered>
  );
}

/** Where the app sends the browser once it has finished pairing. */
export function ConnectedPage({ params }: { params: URLSearchParams }) {
  const name = params.get("name");
  return (
    <Centered>
      <Card>
        <CardHeader className="items-center text-center">
          <CheckCircle2 className="mx-auto size-10 text-success" />
          <CardTitle className="font-serif text-3xl font-normal tracking-[-0.02em]">{name ? `${name} is connected` : "Computer connected"}</CardTitle>
          <CardDescription>You can close this tab. Connect to this computer from your device list, from any browser.</CardDescription>
        </CardHeader>
        <CardContent>
          <Button className="w-full" asChild>
            <a href="#/devices">Go to my devices</a>
          </Button>
        </CardContent>
      </Card>
    </Centered>
  );
}
