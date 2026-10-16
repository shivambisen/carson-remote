import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";

import { api, ApiError, type User } from "@/api";
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
  const [error, setError] = useState<string | null>(null);
  const needsUser = route.page !== "login";

  useEffect(() => {
    if (!needsUser || user) return;
    api.me().then(setUser, (err) => {
      if (err instanceof ApiError && err.status === 401) toLogin();
      else setError(errorMessage(err));
    });
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
  if (!user)
    return (
      <div className="grid h-svh place-items-center">
        <Loader2 className="size-6 animate-spin text-muted-foreground" />
      </div>
    );

  switch (route.page) {
    case "view":
      return <ViewPage key={route.deviceId} deviceId={route.deviceId} forceRelay={route.params.get("relay") === "1"} />;
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
