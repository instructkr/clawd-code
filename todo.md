1. Add Tailwind CSS to Vite correctly: The screenshot `image.png` shows standard HTML with basic browser styles without tailwind classes working correctly, likely because `@tailwindcss/vite` needs to be in `vite.config.ts`, or maybe `src/index.css` is missing the import. Need to fix tailwind setup and check `vite.config.ts`, `src/App.css` and imports.
2. Fix `hardware_daemon.py` sidecar executable: Update the Tauri build process to compile the python script into a binary (e.g. using `pyinstaller`) so it works cross-platform.
3. Fix Settings persistence: Update the Settings modal state to use `localStorage` or `tauri-plugin-store` to keep the user's settings.
4. Implement Ollama model scanning: Add functionality to scan local Ollama instance for models to populate dropdowns when the Ollama provider is selected.
5. Add general settings for language and theme: Add toggles to the settings modal.
