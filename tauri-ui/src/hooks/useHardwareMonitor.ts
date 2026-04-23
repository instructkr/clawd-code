import { useState, useEffect } from "react";
import { Command } from "@tauri-apps/plugin-shell";

export interface HardwareMetrics {
  temp: number;
  vramUsed: number;
  vramTotal: number;
  utilization: number;
}

export function useHardwareMonitor() {
  const [metrics, setMetrics] = useState<HardwareMetrics>({
    temp: 0,
    vramUsed: 0,
    vramTotal: 100,
    utilization: 0,
  });

  useEffect(() => {
    let unmounted = false;
    let process: any = null;

    async function startDaemon() {
      try {
        console.log("Starting hardware daemon...");
        const cmd = Command.sidecar("binaries/hardware_daemon");

        cmd.stdout.on("data", (line) => {
          if (unmounted) return;
          try {
            const parsed = JSON.parse(line);
            if (parsed.type === "hardware_telemetry" && parsed.data) {
              setMetrics({
                temp: parsed.data.temperature_c || 0,
                vramUsed: parsed.data.vram_used_mb || 0,
                vramTotal: parsed.data.vram_total_mb || 100,
                utilization: parsed.data.gpu_utilization_percent || 0,
              });
            }
          } catch (e) {
            // Ignore parse errors from non-JSON stdout
          }
        });

        cmd.stderr.on("data", (line) => {
          console.error("Hardware daemon error:", line);
        });

        process = await cmd.spawn();
      } catch (err) {
        console.error("Failed to start hardware daemon sidecar:", err);
      }
    }

    startDaemon();

    return () => {
      unmounted = true;
      if (process) {
        process.kill();
      }
    };
  }, []);

  return { metrics };
}
