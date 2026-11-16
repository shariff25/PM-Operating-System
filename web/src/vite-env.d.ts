/// <reference types="vite/client" />
interface ImportMetaEnv {
  /** "real" talks to crates/web; anything else (default) uses the in-memory mock. */
  readonly VITE_PMOS_API?: string;
  /** Prefix for backend paths in real mode. Default: same origin, no prefix. */
  readonly VITE_PMOS_API_BASE?: string;
}
