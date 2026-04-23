import { useState } from "react";
import { FolderGit2, MessageSquare, FileCode2, FolderPlus, X } from "lucide-react";
import { useWorkspace } from "../../hooks/useWorkspace";

export default function LeftPanel() {
  const [activeTab, setActiveTab] = useState<"files" | "history">("files");
  const { workspacePath, files, selectWorkspace, closeWorkspace } = useWorkspace();

  return (
    <div className="w-64 bg-mantle border-r border-surface0 flex flex-col h-full flex-shrink-0">
      <div className="flex border-b border-surface0">
        <button
          className={`flex-1 p-3 text-sm font-medium flex items-center justify-center gap-2 transition-colors ${
            activeTab === "files" ? "text-blue border-b-2 border-blue bg-surface0/30" : "text-subtext0 hover:text-text hover:bg-surface0/20"
          }`}
          onClick={() => setActiveTab("files")}
        >
          <FolderGit2 size={16} />
          Workspace
        </button>
        <button
          className={`flex-1 p-3 text-sm font-medium flex items-center justify-center gap-2 transition-colors ${
            activeTab === "history" ? "text-blue border-b-2 border-blue bg-surface0/30" : "text-subtext0 hover:text-text hover:bg-surface0/20"
          }`}
          onClick={() => setActiveTab("history")}
        >
          <MessageSquare size={16} />
          History
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-2">
        {activeTab === "files" ? (
          workspacePath ? (
            <div className="space-y-1">
              <div className="flex items-center justify-between p-1.5 mb-2 bg-surface0/50 rounded text-sm text-text border border-surface1">
                <span className="truncate font-medium">{workspacePath.split(/[\/\\]/).pop()}</span>
                <button onClick={closeWorkspace} className="text-subtext0 hover:text-red transition-colors p-1" title="Close Workspace">
                  <X size={14} />
                </button>
              </div>

              {files.map((file, i) => (
                <div key={i} className="flex items-center gap-2 p-1.5 rounded hover:bg-surface0 cursor-pointer text-sm text-subtext1">
                  {file.isDir ? <FolderGit2 size={14} className="text-blue" /> : <FileCode2 size={14} className="text-mauve" />}
                  <span className="truncate">{file.name}</span>
                </div>
              ))}
              <div className="pt-2 text-xs text-subtext0 italic px-2">
                Note: Directory traversal requires full tauri-plugin-fs integration.
              </div>
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center h-full text-center p-4 space-y-4">
              <div className="text-subtext0 text-sm">No workspace selected.</div>
              <button
                onClick={selectWorkspace}
                className="flex items-center gap-2 px-4 py-2 bg-blue text-crust font-medium rounded-md hover:bg-blue/90 transition-colors text-sm"
              >
                <FolderPlus size={16} />
                Open Folder
              </button>
            </div>
          )
        ) : (
          <div className="space-y-2 p-1">
            <div className="p-2 rounded bg-surface0/50 hover:bg-surface0 cursor-pointer">
              <div className="text-sm font-medium text-text truncate">Implement UI layout</div>
              <div className="text-xs text-subtext0 mt-1">2 hours ago</div>
            </div>
            <div className="p-2 rounded bg-surface0/50 hover:bg-surface0 cursor-pointer">
              <div className="text-sm font-medium text-text truncate">Fix bug in parsing</div>
              <div className="text-xs text-subtext0 mt-1">Yesterday</div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
