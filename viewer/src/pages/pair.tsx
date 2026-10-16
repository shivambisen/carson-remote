import { useState, type FormEvent } from "react";
import { toast } from "sonner";

import { api, type PairingPreview } from "@/api";
import { osLabel } from "@/format";
import { errorMessage } from "@/lib/errors";
import { navigate } from "@/lib/router";
import { formatCode, isCompleteCode } from "@/pairing";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";

/**
 * Join with a code: the person at the other computer chooses "Connect with a code" in glenley
 * (or runs `glenley-agent pair --code`) and reads out the code; entering it here adds that
 * computer to this account, for good or just for one session.
 */
export function PairPage({ params }: { params: URLSearchParams }) {
  const [code, setCode] = useState(formatCode(params.get("code") ?? ""));
  const [preview, setPreview] = useState<PairingPreview | null>(null);
  const [error, setError] = useState<string | null>(null);

  const lookup = (e: FormEvent) => {
    e.preventDefault();
    if (!isCompleteCode(code)) return setError("Enter the 8-letter code shown on that computer.");
    setError(null);
    api.pairLookup(code).then(setPreview, (err) => setError(errorMessage(err)));
  };
  const approve = () =>
    api.pairApprove(code).then(
      (d) => {
        toast.success(`Added ${d.name}. It shows as online in a few seconds: then click Connect.`);
        navigate("#/devices");
      },
      (err) => setError(errorMessage(err)),
    );

  return (
    <div className="mx-auto max-w-md">
      <Card>
        <CardHeader>
          <CardTitle className="font-serif text-3xl font-normal tracking-[-0.02em]">Enter a code</CardTitle>
          <CardDescription>
            On the other computer, open the glenley menu and choose <strong>Connect with a code</strong>. Type the code it shows.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {preview ? (
            <>
              <p className="text-sm">
                Connect to <strong>{preview.name}</strong> ({osLabel(preview.os)})?{" "}
                {preview.temporary
                  ? "It was shared just this once: it leaves your account when you disconnect."
                  : "It stays in your devices until you or they remove it."}
              </p>
              <p className="text-xs text-muted-foreground">Only continue with a code the person at that computer gave you.</p>
              <div className="flex gap-2">
                <Button onClick={() => void approve()}>Add computer</Button>
                <Button variant="outline" onClick={() => setPreview(null)}>
                  Cancel
                </Button>
              </div>
            </>
          ) : (
            <form className="grid gap-3" onSubmit={lookup}>
              <Input
                autoFocus
                aria-label="Pairing code"
                placeholder="BCDF-GHJK"
                autoComplete="off"
                autoCapitalize="characters"
                spellCheck={false}
                className="h-12 text-center font-mono text-2xl tracking-[0.3em] md:text-2xl"
                value={code}
                onChange={(e) => setCode(formatCode(e.target.value))}
              />
              <Button type="submit">Continue</Button>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
