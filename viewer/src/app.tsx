import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api, ApiError, type Billing, type User } from "@/api";
import { Shell } from "@/components/shell";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { errorMessage } from "@/lib/errors";
import { navigate, toLogin, useRoute } from "@/lib/router";
import { ActivityPage } from "@/pages/activity";
import { ConnectedPage, ConnectPage } from "@/pages/connect";
import { DevicesPage } from "@/pages/devices";
import { LoginPage } from "@/pages/login";
import { PairPage } from "@/pages/pair";
import { Paywall } from "@/pages/paywall";
import { ViewPage } from "@/pages/view";

export function App() {
  return (
    <TooltipProvider delayDuration={300}>
      <Routes />
      <Toaster position="bottom-right" />
    </TooltipProvider>
  );
}

function Routes() {
  const route = useRoute();
  const [user, setUser] = useState<User | null>(null);
  const [billing, setBilling] = useState<Billing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const needsUser = route.page !== "login";

  useEffect(() => {
    if (!needsUser || user) return;
    Promise.all([api.me(), api.billing()]).then(
      ([u, b]) => {
        setUser(u);
        setBilling(b);
      },
      (err) => {
        if (err instanceof ApiError && err.status === 401) toLogin();
        else setError(errorMessage(err));
      },
    );
  }, [needsUser, user]);

  const signOut = () =>
    void api
      .logout()
      .catch(() => {})
      .then(() => {
        setUser(null);
        navigate("#/login");
      });

  if (route.page === "login") return <LoginPage params={route.params} />;
  if (error) return <p className="p-8 text-sm text-destructive">{error}</p>;
  if (!user || !billing)
    return (
      <div className="grid h-svh place-items-center">
        <Loader2 className="size-6 animate-spin text-muted-foreground" />
      </div>
    );

  // Everything but the activity log needs the one-time purchase (when payments are on).
  if (billing.enabled && !billing.paid && route.page !== "activity" && route.page !== "connected")
    return <Paywall billing={billing} user={user} onPaid={() => setBilling({ ...billing, paid: true })} onSignOut={signOut} />;

  switch (route.page) {
    case "view":
      // Keyed on the relay flag too: switching to the relay starts a fresh session.
      return <ViewPage key={`${route.deviceId}:${route.params.get("relay") ?? ""}`} deviceId={route.deviceId} forceRelay={route.params.get("relay") === "1"} />;
    case "connect":
      return <ConnectPage params={route.params} />;
    case "connected":
      return <ConnectedPage params={route.params} />;
    case "pair":
      return (
        <Shell user={user} onSignOut={signOut}>
          <PairPage params={route.params} />
        </Shell>
      );
    case "activity":
      return (
        <Shell user={user} onSignOut={signOut} active="activity">
          <ActivityPage />
        </Shell>
      );
    default:
      return (
        <Shell user={user} onSignOut={signOut} active="devices">
          <DevicesPage />
        </Shell>
      );
  }
}
