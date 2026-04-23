import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';

export interface WorkspaceFile {
  name: string;
  path: string;
  isDir: boolean;
  children?: WorkspaceFile[];
}

export function useWorkspace() {
  const [workspacePath, setWorkspacePath] = useState<string | null>(null);
  const [files, setFiles] = useState<WorkspaceFile[]>([]);

  const selectWorkspace = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
      });
      if (selected && typeof selected === 'string') {
        setWorkspacePath(selected);
        // Simple mock for now, ideally we'd recursively read using tauri-plugin-fs
        const dirName = selected.split(/[\/\\]/).pop() || selected;
        setFiles([{ name: dirName, path: selected, isDir: true }]);
      }
    } catch (e) {
      console.error("Failed to select workspace:", e);
    }
  };

  const closeWorkspace = () => {
    setWorkspacePath(null);
    setFiles([]);
  };

  return { workspacePath, files, selectWorkspace, closeWorkspace };
}
