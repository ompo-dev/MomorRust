import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { serverAudit } from './server-audit.ts';

export default defineConfig({
  plugins: [react(), tailwindcss(), serverAudit()],
  build: {
    rolldownOptions: { input: ['index.html', 'frame.html'] },
  },
});
