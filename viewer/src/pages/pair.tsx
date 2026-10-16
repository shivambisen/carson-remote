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

/** Fallback for computers without a browser: approve the code `carson-agent pair` shows. */
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
        toast.success(`Added ${d.name}`);
        navigate("#/devices");
      },
      (err) => setError(errorMessage(err)),
    );

  return (
    <div className="mx-auto max-w-md">
      <Card>
        <CardHeader>
          <CardTitle>Add with a code</CardTitle>
          <CardDescription>For computers without a browser: enter the code that carson shows there.</CardDescription>
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
                Add <strong>{preview.name}</strong> ({osLabel(preview.os)}) to your account? Only approve a code you started yourself.
              </p>
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
