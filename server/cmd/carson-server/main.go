// Command carson-server runs the carson signaling, device registry and control server.
package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"carson/server/internal/config"
	"carson/server/internal/httpapi"
	"carson/server/internal/store"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "carson-server:", err)
		os.Exit(1)
	}
}

func run() error {
	cfg, err := config.Load(os.Getenv)
	if err != nil {
		return fmt.Errorf("config: %w", err)
	}
	log := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: cfg.LogLevel}))
	if cfg.Dev {
		log.Warn("dev mode enabled: email-only dev login is ON; do not use in production")
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	st, err := store.Open(ctx, cfg.DBPath)
	if err != nil {
		return fmt.Errorf("open database: %w", err)
	}
	defer st.Close()
	go cleanup(ctx, st, log, cfg.AuditRetention)

	api := httpapi.New(cfg, st, log)
	srv := &http.Server{
		Addr:              cfg.Addr,
		Handler:           api.Handler,
		ReadHeaderTimeout: 10 * time.Second,
	}

	errc := make(chan error, 1)
	go func() {
		log.Info("listening", "addr", cfg.Addr, "public_url", cfg.PublicURL.String(), "version", httpapi.Version)
		errc <- srv.ListenAndServe()
	}()

	select {
	case err := <-errc:
		return err
	case <-ctx.Done():
	}

	log.Info("shutting down")
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := srv.Shutdown(shutdownCtx); err != nil && !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}

// cleanup periodically deletes expired sessions, logins, pairings and tokens.
func cleanup(ctx context.Context, st *store.Store, log *slog.Logger, auditRetention time.Duration) {
	t := time.NewTicker(10 * time.Minute)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			if err := st.DeleteExpired(ctx, time.Now()); err != nil {
				log.Error("cleanup expired rows", "err", err)
			}
			if err := st.PruneAudit(ctx, time.Now().Add(-auditRetention)); err != nil {
				log.Error("prune audit log", "err", err)
			}
		}
	}
}
