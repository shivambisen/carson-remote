import { useEffect, useState, type FormEvent } from "react";

import { api, loginUrl, type Providers } from "@/api";
import { Centered } from "@/components/shell";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { errorMessage } from "@/lib/errors";
import { hashOf, navigate } from "@/lib/router";

const ERRORS: Record<string, string> = {
  invalid_state: "Your sign-in expired or was already used. Please try again.",
  access_denied: "Sign-in was cancelled.",
  unverified_email: "Your account needs a verified email address.",
  login_failed: "Sign-in failed. Please try again.",
};

function GoogleIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path fill="#4285F4" d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92a5.06 5.06 0 0 1-2.2 3.32v2.77h3.57c2.08-1.92 3.27-4.74 3.27-8.1z" />
      <path fill="#34A853" d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84A11 11 0 0 0 12 23z" />
      <path fill="#FBBC05" d="M5.84 14.1A6.6 6.6 0 0 1 5.5 12c0-.73.13-1.44.34-2.1V7.06H2.18a11 11 0 0 0 0 9.88l3.66-2.84z" />
      <path fill="#EA4335" d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15A10.96 10.96 0 0 0 12 1 11 11 0 0 0 2.18 7.06l3.66 2.84C6.71 7.31 9.14 5.38 12 5.38z" />
    </svg>
  );
}

function GitHubIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" fill="currentColor">
      <path d="M12 .5a11.5 11.5 0 0 0-3.64 22.41c.58.1.79-.25.79-.56v-2c-3.2.7-3.88-1.37-3.88-1.37-.52-1.33-1.28-1.69-1.28-1.69-1.05-.71.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.77 2.7 1.26 3.36.96.1-.75.4-1.26.73-1.55-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.19-3.1-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.17 1.18a11 11 0 0 1 5.77 0c2.2-1.49 3.17-1.18 3.17-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.84 1.19 3.1 0 4.42-2.7 5.39-5.26 5.68.41.36.78 1.06.78 2.14v3.17c0 .31.21.67.8.56A11.5 11.5 0 0 0 12 .5z" />
    </svg>
  );
}

export function LoginPage({ params }: { params: URLSearchParams }) {
  const returnTo = params.get("next") ?? "/#/devices";
  const errorCode = params.get("error");
  const [providers, setProviders] = useState<Providers | null>(null);
  const [error, setError] = useState<string | null>(errorCode ? (ERRORS[errorCode] ?? "Sign-in failed.") : null);
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.providers().then(setProviders, (e) => setError(errorMessage(e)));
  }, []);

  const devLogin = (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    api.devLogin(email).then(
      () => {
        navigate(hashOf(returnTo));
        location.reload(); // pick up the new session everywhere
      },
      (err) => {
        setBusy(false);
        setError(errorMessage(err));
      },
    );
  };

  return (
    <Centered>
      <Card>
        <CardHeader className="text-center">
          <CardTitle className="font-serif text-3xl font-normal tracking-[-0.02em]">Welcome to glenley</CardTitle>
          <CardDescription>Sign in to reach your computers from anywhere.</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {providers?.providers.map((p) => (
            <Button key={p} variant="outline" size="lg" className="w-full" asChild>
              <a href={loginUrl(p, returnTo)}>
                {p === "github" ? <GitHubIcon /> : <GoogleIcon />}
                Continue with {p === "github" ? "GitHub" : "Google"}
              </a>
            </Button>
          ))}
          {providers && providers.providers.length === 0 && !providers.dev && (
            <p className="text-center text-sm text-destructive">No sign-in method is configured on this server.</p>
          )}
          {providers?.dev && (
            <>
              {providers.providers.length > 0 && <Separator />}
              <form className="grid gap-3" onSubmit={devLogin}>
                <div className="grid gap-2">
                  <Label htmlFor="email">Email</Label>
                  <Input id="email" type="email" required autoComplete="email" placeholder="you@example.com" value={email} onChange={(e) => setEmail(e.target.value)} />
                </div>
                <Button type="submit" disabled={busy}>
                  Sign in
                </Button>
                <p className="text-center text-xs text-muted-foreground">Test server: any email works, nothing is verified.</p>
              </form>
            </>
          )}
        </CardContent>
      </Card>
      <p className="mt-6 text-center text-xs text-muted-foreground">Free and open source. Your screen is end-to-end encrypted.</p>
    </Centered>
  );
}
