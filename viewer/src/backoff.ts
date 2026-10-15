/** Jittered exponential backoff, mirroring the agent: delay in [d/2, d], d = min(cap, base·2^n). */
export class Backoff {
  private attempt = 0;

  constructor(
    private readonly baseMs = 500,
    private readonly capMs = 10_000,
    private readonly random: () => number = Math.random,
  ) {}

  next(): number {
    const d = Math.min(this.capMs, this.baseMs * 2 ** Math.min(this.attempt, 30));
    this.attempt++;
    return d / 2 + (d / 2) * this.random();
  }

  reset(): void {
    this.attempt = 0;
  }
}
