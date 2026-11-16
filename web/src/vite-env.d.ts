/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_API?: 'mock' | 'real';
  readonly VITE_MOCK_BRIEF?: 'on' | 'off';
  readonly VITE_MOCK_HOSTILE?: 'on' | 'off';
  readonly VITE_MOCK_FAST?: 'on' | 'off';
  readonly VITE_AUTH_PROVIDER?: 'dev' | 'entra';
}
interface ImportMeta {
  readonly env: ImportMetaEnv & Record<string, string | boolean | undefined>;
}
