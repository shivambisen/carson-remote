import { useState } from "react";
import { Check, Loader2, ShieldCheck } from "lucide-react";

import { api, type Billing, type CheckoutResult, type User } from "@/api";
import { Centered } from "@/components/shell";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { errorMessage } from "@/lib/errors";

interface RazorpayOptions {
  key: string;
  order_id: string;
  amount: number;
  currency: string;
  name: string;
  description: string;
  prefill: { email: string; name: string };
  theme: { color: string };
  handler: (r: CheckoutResult) => void;
  modal: { ondismiss: () => void };
}
type RazorpayCtor = new (o: RazorpayOptions) => { open: () => void; on: (e: string, f: (r: { error?: { description?: string } }) => void) => void };

/** Loads Razorpay Checkout once (only when someone is about to pay). */
function loadCheckout(): Promise<RazorpayCtor> {
  const w = window as Window & { Razorpay?: RazorpayCtor };
  if (w.Razorpay) return Promise.resolve(w.Razorpay);
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = "https://checkout.razorpay.com/v1/checkout.js";
    s.onload = () => (w.Razorpay ? resolve(w.Razorpay) : reject(new Error("Payment window did not load")));
    s.onerror = () => reject(new Error("Couldn't reach Razorpay. Check your connection and try again."));
    document.head.append(s);
  });
}

export function price(b: Pick<Billing, "amount" | "currency">): string {
  const value = b.amount / 100;
  return new Intl.NumberFormat("en-IN", { style: "currency", currency: b.currency, maximumFractionDigits: value % 1 ? 2 : 0 }).format(value);
}

/** Shown to signed-in accounts that haven't bought glenley yet. */
export function Paywall({ billing, user, onPaid, onSignOut }: { billing: Billing; user: User; onPaid: () => void; onSignOut: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pay = async () => {
    setBusy(true);
    setError(null);
    try {
      const [Razorpay, order] = await Promise.all([loadCheckout(), api.billingOrder()]);
      const checkout = new Razorpay({
        key: order.key_id,
        order_id: order.order_id,
        amount: order.amount,
        currency: order.currency,
        name: "glenley",
        description: "Lifetime access",
        prefill: { email: order.email, name: order.name },
        theme: { color: "#0e1229" },
        handler: (result) => {
          api.billingVerify(result).then(onPaid, (e) => {
            setBusy(false);
            setError(`${errorMessage(e)} If money was taken, it will be matched automatically within a few minutes.`);
          });
        },
        modal: { ondismiss: () => setBusy(false) },
      });
      checkout.on("payment.failed", (r) => {
        setBusy(false);
        setError(r.error?.description ?? "The payment didn't go through. You weren't charged.");
      });
      checkout.open();
    } catch (e) {
      setBusy(false);
      setError(errorMessage(e));
    }
  };

  return (
    <Centered>
      <Card>
        <CardHeader className="text-center">
          <CardTitle className="font-serif text-3xl font-normal tracking-[-0.02em]">Get glenley</CardTitle>
          <CardDescription>One payment, yours for life. No subscription.</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-5">
          <div className="text-center">
            <span className="font-serif text-5xl">{price(billing)}</span>
            <span className="ml-2 text-sm text-muted-foreground">once</span>
          </div>
          <ul className="grid gap-2 text-sm">
            {["Every computer you own, from any browser", "Mac and Windows apps", "End-to-end encrypted", "All future updates"].map((t) => (
              <li key={t} className="flex items-center gap-2">
                <Check className="size-4 text-success" /> {t}
              </li>
            ))}
          </ul>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          <Button size="lg" className="w-full" disabled={busy} onClick={() => void pay()}>
            {busy ? <Loader2 className="animate-spin" /> : null}
            Pay {price(billing)} · UPI, cards, netbanking
          </Button>
          <p className="flex items-center justify-center gap-1.5 text-center text-xs text-muted-foreground">
            <ShieldCheck className="size-3.5" /> Secure payment by Razorpay
          </p>
          <p className="text-center text-xs text-muted-foreground">
            Signed in as {user.email} ·{" "}
            <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={onSignOut}>
              Sign out
            </button>
          </p>
        </CardContent>
      </Card>
    </Centered>
  );
}
