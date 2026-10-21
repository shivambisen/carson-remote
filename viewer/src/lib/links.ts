/**
 * Where people download the glenley app for the computer they want to reach. When this viewer is
 * opened on its fallback address (networks that block glenley.in), link to the landing page's
 * fallback address too.
 */
export const DOWNLOADS_URL =
  typeof location !== "undefined" && !location.hostname.endsWith("glenley.in") && location.hostname !== "localhost"
    ? "https://glenley.vercel.app/#download"
    : "https://glenley.in/#download";
