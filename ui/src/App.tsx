import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { JSX } from "preact";

type Workflow = {
  name: string;
  image: string;
  command: boolean;
  depends_on: string[];
};

type RenderedWorkflow = Workflow & {
  index: number;
  status: string;
};

type PipelineResponse = {
  name: string;
  workflows: Workflow[];
};

type CommandResponse = {
  message: string;
};

type RuntimeChoice = "auto" | "docker" | "podman" | "apple-container";

type RuntimeOption = {
  value: RuntimeChoice;
  label: string;
};

type EventKind =
  | "pipeline-started"
  | "pipeline-finished"
  | "pipeline-cancelled"
  | "workflow-status"
  | "log"
  | "error";

type PipelineEvent = {
  kind: EventKind;
  timestamp_ms: number;
  workflow?: string | null;
  phase?: string | null;
  status?: string | null;
  message: string;
};

type ConnectionState = "Connecting..." | "Live" | "Reconnecting...";

type LogLevel = "pipeline" | "log" | "control" | "error" | "status";

type LogEntry = {
  id: number;
  ts: number;
  level: LogLevel;
  prefix: string;
  message: string;
};

type View = "live" | "history";

type RunRecord = {
  id: number;
  started_at: number;
  finished_at: number | null;
  mode: string;
  workflow: string | null;
  status: string;
};

type RunEventRecord = {
  id: number;
  run_id: number;
  timestamp_ms: number;
  kind: EventKind;
  workflow: string | null;
  phase: string | null;
  status: string | null;
  message: string;
};

type StatsSummary = {
  ok: number;
  fail: number;
  run: number;
  pend: number;
  idle: number;
  skip: number;
  total: number;
  waiting: number;
};

const SIGNAL_BY_CONN: Record<ConnectionState, { code: string; klass: string }> =
  {
    "Connecting...": { code: "CON", klass: "signal--warn" },
    Live: { code: "LIV", klass: "signal--ok" },
    "Reconnecting...": { code: "RCN", klass: "signal--err" },
  };

const MAX_LOGS = 800;

const BASE_RUNTIME_OPTIONS: RuntimeOption[] = [
  { value: "auto", label: "[ AUTO ]" },
  { value: "docker", label: "Docker" },
  { value: "podman", label: "Podman" },
];

const MACOS_RUNTIME_OPTIONS: RuntimeOption[] = [
  { value: "auto", label: "[ AUTO ]" },
  { value: "apple-container", label: "Apple container" },
  { value: "docker", label: "Docker" },
  { value: "podman", label: "Podman" },
];

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function pad3(n: number): string {
  return String(n).padStart(3, "0");
}

function pad4(n: number): string {
  return String(n).padStart(4, "0");
}

function formatTimestamp(ms: number): string {
  const d = new Date(ms);
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}.${pad3(d.getMilliseconds())}`;
}

function formatRunStarted(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}.${pad2(d.getMonth() + 1)}.${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`;
}

function formatDuration(startedAt: number, finishedAt: number | null): string {
  if (!finishedAt) return "—";
  const ms = Math.max(0, finishedAt - startedAt);
  if (ms < 1000) return `${ms}ms`;
  const totalSeconds = Math.floor(ms / 1000);
  const seconds = totalSeconds % 60;
  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes === 0) {
    const tenths = Math.floor((ms % 1000) / 100);
    return `${seconds}.${tenths}s`;
  }
  const minutes = totalMinutes % 60;
  const hours = Math.floor(totalMinutes / 60);
  if (hours === 0) {
    return `${minutes}m ${pad2(seconds)}s`;
  }
  return `${hours}h ${pad2(minutes)}m`;
}

function levelForRunEvent(event: RunEventRecord): LogLevel {
  if (event.kind === "error") return "error";
  if (
    event.kind === "pipeline-started" ||
    event.kind === "pipeline-finished" ||
    event.kind === "pipeline-cancelled"
  )
    return "pipeline";
  if (event.kind === "workflow-status") return "status";
  return "log";
}

function runStatusClass(status: string): string {
  switch (status) {
    case "succeeded":
      return "run-pill--ok";
    case "failed":
      return "run-pill--fail";
    case "running":
      return "run-pill--run";
    case "cancelled":
      return "run-pill--warn";
    default:
      return "run-pill--idle";
  }
}

function buildPrefix(
  phase: string | null | undefined,
  workflow: string | null | undefined,
): string {
  const p = (phase || "phase").toUpperCase();
  const w = workflow || "—";
  return `${p} · ${w}`;
}

function detectBrowserPlatform(): "macos" | "other" {
  const nav = navigator as Navigator & {
    userAgentData?: { platform?: string };
  };
  const platform = `${nav.userAgentData?.platform || navigator.platform || navigator.userAgent}`;
  return /mac/i.test(platform) ? "macos" : "other";
}

function runtimeLabel(runtime: RuntimeChoice): string {
  switch (runtime) {
    case "apple-container":
      return "APPLE";
    case "docker":
      return "DOCKER";
    case "podman":
      return "PODMAN";
    default:
      return "AUTO";
  }
}

let nextLogId = 1;

export default function App(): JSX.Element {
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [statuses, setStatuses] = useState<Map<string, string>>(
    () => new Map(),
  );
  const [running, setRunning] = useState(false);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [selectedRuntime, setSelectedRuntime] = useState<RuntimeChoice>("auto");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [connectionState, setConnectionState] =
    useState<ConnectionState>("Connecting...");
  const [now, setNow] = useState(() => new Date());
  const [projectName, setProjectName] = useState("my-ci");
  const [eventCount, setEventCount] = useState(0);
  const [autoScroll, setAutoScroll] = useState(true);

  const [view, setView] = useState<View>("live");
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [runsLoading, setRunsLoading] = useState(false);
  const [runsError, setRunsError] = useState<string | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<number | null>(null);
  const [runEvents, setRunEvents] = useState<RunEventRecord[]>([]);
  const [runEventsLoading, setRunEventsLoading] = useState(false);
  const [runEventsError, setRunEventsError] = useState<string | null>(null);

  const logsRef = useRef<HTMLDivElement>(null);
  const runEventsRef = useRef<HTMLDivElement>(null);
  const browserPlatform = useMemo(() => detectBrowserPlatform(), []);
  const runtimeOptions =
    browserPlatform === "macos" ? MACOS_RUNTIME_OPTIONS : BASE_RUNTIME_OPTIONS;

  const renderedWorkflows = useMemo<RenderedWorkflow[]>(
    () =>
      workflows.map((wf, i) => ({
        ...wf,
        index: i,
        status: statuses.get(wf.name) || "idle",
      })),
    [workflows, statuses],
  );

  const stats = useMemo<StatsSummary>(() => {
    let ok = 0;
    let fail = 0;
    let run = 0;
    let pend = 0;
    let idle = 0;
    let skip = 0;
    for (const wf of renderedWorkflows) {
      switch (wf.status) {
        case "succeeded":
          ok++;
          break;
        case "failed":
          fail++;
          break;
        case "running":
          run++;
          break;
        case "pending":
          pend++;
          break;
        case "skipped":
          skip++;
          break;
        default:
          idle++;
      }
    }
    return {
      ok,
      fail,
      run,
      pend,
      idle,
      skip,
      total: renderedWorkflows.length,
      waiting: idle + pend + skip,
    };
  }, [renderedWorkflows]);

  function appendLog(
    level: LogLevel,
    prefix: string,
    message: string,
    ts?: number,
  ): void {
    setLogs((prev) => {
      const entry: LogEntry = {
        id: nextLogId++,
        ts: ts ?? Date.now(),
        level,
        prefix,
        message,
      };
      if (prev.length >= MAX_LOGS) {
        const next = prev.slice(prev.length - (MAX_LOGS - 1));
        next.push(entry);
        return next;
      }
      return prev.concat(entry);
    });
    setEventCount((c) => c + 1);
  }

  useEffect(() => {
    const i = window.setInterval(() => setNow(new Date()), 1000);
    return () => window.clearInterval(i);
  }, []);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await fetch("/api/pipeline");
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = (await res.json()) as PipelineResponse;
        if (cancelled) return;
        setWorkflows(data.workflows);
        setProjectName(data.name);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        appendLog("error", "BOOT", `Failed to load pipeline: ${msg}`);
      }
    })();

    const source = new EventSource("/api/events");

    source.addEventListener("open", () => setConnectionState("Live"));
    source.addEventListener("error", () =>
      setConnectionState("Reconnecting..."),
    );

    source.addEventListener("pipeline", (msg) => {
      const event = JSON.parse(
        (msg as MessageEvent<string>).data,
      ) as PipelineEvent;
      const ts = event.timestamp_ms || Date.now();
      const kind = event.kind;

      if (kind === "pipeline-started") {
        setRunning(true);
        appendLog("pipeline", "PIPELINE", event.message, ts);
        return;
      }

      if (kind === "pipeline-finished" || kind === "pipeline-cancelled") {
        setRunning(false);
        appendLog("pipeline", "PIPELINE", event.message, ts);
        return;
      }

      if (kind === "workflow-status") {
        if (event.workflow && event.status) {
          const wfName = event.workflow;
          const wfStatus = event.status;
          setStatuses((prev) => {
            const n = new Map(prev);
            n.set(wfName, wfStatus);
            return n;
          });
        }
        appendLog(
          "status",
          buildPrefix(event.phase, event.workflow),
          event.message,
          ts,
        );
        return;
      }

      if (kind === "log") {
        appendLog(
          "log",
          buildPrefix(event.phase, event.workflow),
          event.message,
          ts,
        );
        return;
      }

      if (kind === "error") {
        setRunning(false);
        if (event.workflow && event.status) {
          const wfName = event.workflow;
          const wfStatus = event.status;
          setStatuses((prev) => {
            const n = new Map(prev);
            n.set(wfName, wfStatus);
            return n;
          });
        }
        appendLog("error", "ERROR", event.message, ts);
      }
    });

    return () => {
      cancelled = true;
      source.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!autoScroll) return;
    const el = logsRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [logs, autoScroll]);

  async function loadRuns(): Promise<void> {
    setRunsLoading(true);
    setRunsError(null);
    try {
      const res = await fetch("/api/runs?limit=50");
      if (res.status === 503) {
        setRunsError("History disabled (no database).");
        setRuns([]);
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as RunRecord[];
      setRuns(data);
      setSelectedRunId((current) => {
        if (current != null && data.some((r) => r.id === current)) {
          return current;
        }
        return data[0]?.id ?? null;
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setRunsError(msg);
    } finally {
      setRunsLoading(false);
    }
  }

  async function loadRunEvents(runId: number): Promise<void> {
    setRunEventsLoading(true);
    setRunEventsError(null);
    try {
      const res = await fetch(`/api/runs/${runId}/events`);
      if (res.status === 503) {
        setRunEventsError("History disabled (no database).");
        setRunEvents([]);
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = (await res.json()) as RunEventRecord[];
      setRunEvents(data);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setRunEventsError(msg);
    } finally {
      setRunEventsLoading(false);
    }
  }

  useEffect(() => {
    if (view !== "history") return;
    void loadRuns();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view]);

  useEffect(() => {
    if (view !== "history") return;
    if (selectedRunId == null) {
      setRunEvents([]);
      return;
    }
    void loadRunEvents(selectedRunId);
  }, [view, selectedRunId]);

  useEffect(() => {
    const el = runEventsRef.current;
    if (!el) return;
    el.scrollTop = 0;
  }, [selectedRunId]);

  useEffect(() => {
    if (view !== "history") return;
    if (running) return;
    void loadRuns();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);

  async function command(path: string): Promise<void> {
    const workflow = selectedWorkflow || null;
    try {
      const res = await fetch(path, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ workflow, runtime: selectedRuntime }),
      });
      const data = (await res.json()) as CommandResponse;
      appendLog("control", "CTRL", data.message);
      if (path === "/api/build" || path === "/api/run") {
        setView("live");
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      appendLog("error", "CTRL", msg);
    }
  }

  function clearLogs(): void {
    setLogs([]);
  }

  function onLogsScroll(e: JSX.TargetedEvent<HTMLDivElement>): void {
    const el = e.currentTarget;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    setAutoScroll(distance < 24);
  }

  const sig = SIGNAL_BY_CONN[connectionState];
  const sysClass = running ? "signal--run" : "signal--idle";
  const sysCode = running ? "RUN" : "IDL";
  const clockText = `${pad2(now.getHours())}:${pad2(now.getMinutes())}:${pad2(now.getSeconds())}`;
  const dateText = `${now.getFullYear()}.${pad2(now.getMonth() + 1)}.${pad2(now.getDate())}`;
  const sigKvClass =
    connectionState === "Live"
      ? "kv__v--ok"
      : connectionState === "Reconnecting..."
        ? "kv__v--err"
        : "kv__v--warn";

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brand__mark" aria-hidden="true">
            <span className="brand__mark-core" />
          </div>
          <div className="brand__text">
            <div className="brand__title">
              <span className="brand__name">MY-CI</span>
              <span className="brand__sep">//</span>
              <span className="brand__sub">OPERATOR CONSOLE</span>
            </div>
            <div className="brand__meta">
              <span className="brand__proj">{projectName}</span>
              <span className="brand__bullet">·</span>
              <span>{pad2(stats.total)} WORKFLOWS</span>
              <span className="brand__bullet">·</span>
              <span>EV {pad4(eventCount)}</span>
            </div>
          </div>
        </div>

        <div
          className="topbar__signals"
          role="group"
          aria-label="System signals"
        >
          <div className={`signal ${sig.klass}`}>
            <span className="signal__dot" aria-hidden="true" />
            <span className="signal__label">SIG</span>
            <span className="signal__code">{sig.code}</span>
          </div>
          <div className={`signal ${sysClass}`}>
            <span className="signal__dot" aria-hidden="true" />
            <span className="signal__label">SYS</span>
            <span className="signal__code">{sysCode}</span>
          </div>
        </div>

        <div className="topbar__clock" aria-label="Console clock">
          <div className="clock__date">{dateText}</div>
          <div className="clock__time">
            <span className="clock__t">T</span>
            <span className="clock__digits">{clockText}</span>
          </div>
        </div>
      </header>

      <div className="strip">
        <div className="strip__group">
          <span className="strip__label">VIEW</span>
          <div className="tabs" role="tablist" aria-label="Console view">
            <button
              type="button"
              role="tab"
              aria-selected={view === "live"}
              className={`tab${view === "live" ? " tab--active" : ""}`}
              onClick={() => setView("live")}
            >
              LIVE
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={view === "history"}
              className={`tab${view === "history" ? " tab--active" : ""}`}
              onClick={() => setView("history")}
            >
              HISTORY
            </button>
          </div>
        </div>
        <div className="strip__group">
          <span className="strip__label">TARGET</span>
          <div className="select">
            <select
              value={selectedWorkflow}
              onChange={(event) =>
                setSelectedWorkflow(
                  (event.currentTarget as HTMLSelectElement).value,
                )
              }
            >
              <option value="">[ ALL WORKFLOWS ]</option>
              {workflows.map((wf) => (
                <option key={wf.name} value={wf.name}>
                  {wf.name}
                </option>
              ))}
            </select>
            <span className="select__caret" aria-hidden="true">
              ▾
            </span>
          </div>
          <span className="strip__hint">
            {selectedWorkflow
              ? `scoped: ${selectedWorkflow}`
              : "scope: all runnable"}
          </span>
        </div>
        <div className="strip__group">
          <span className="strip__label">RUNTIME</span>
          <div className="select select--runtime">
            <select
              value={selectedRuntime}
              disabled={running}
              onChange={(event) =>
                setSelectedRuntime(
                  (event.currentTarget as HTMLSelectElement)
                    .value as RuntimeChoice,
                )
              }
            >
              {runtimeOptions.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
            <span className="select__caret" aria-hidden="true">
              ▾
            </span>
          </div>
          <span className="strip__hint">
            {browserPlatform === "macos"
              ? "platform: macos"
              : "platform: generic"}
          </span>
        </div>
        <div className="strip__group strip__group--right">
          <button
            type="button"
            className="btn btn--ghost"
            disabled={running}
            onClick={() => command("/api/build")}
          >
            BUILD
          </button>
          <button
            type="button"
            className="btn btn--primary"
            disabled={running}
            onClick={() => command("/api/run")}
          >
            <span className="btn__caret" aria-hidden="true">
              ▸
            </span>
            RUN
          </button>
          <button
            type="button"
            className="btn btn--danger"
            disabled={!running}
            onClick={() => command("/api/stop")}
          >
            <span className="btn__square" aria-hidden="true" />
            STOP
          </button>
          <button type="button" className="btn btn--ghost" onClick={clearLogs}>
            CLEAR
          </button>
        </div>
      </div>

      <main className="main">
        {view === "history" ? (
          <HistoryView
            runs={runs}
            runsLoading={runsLoading}
            runsError={runsError}
            selectedRunId={selectedRunId}
            setSelectedRunId={setSelectedRunId}
            runEvents={runEvents}
            runEventsLoading={runEventsLoading}
            runEventsError={runEventsError}
            onRefreshRuns={() => void loadRuns()}
            runEventsRef={runEventsRef}
          />
        ) : (
          <LiveView
            renderedWorkflows={renderedWorkflows}
            stats={stats}
            logs={logs}
            logsRef={logsRef}
            onLogsScroll={onLogsScroll}
            autoScroll={autoScroll}
            clockText={clockText}
          />
        )}
      </main>

      <footer className="statusbar">
        <div className="statusbar__group">
          <span className="kv">
            <span className="kv__k">PROJ</span>
            <span className="kv__v">{projectName}</span>
          </span>
          <span className="kv">
            <span className="kv__k">SCOPE</span>
            <span className="kv__v">{selectedWorkflow || "ALL"}</span>
          </span>
          <span className="kv">
            <span className="kv__k">RT</span>
            <span className="kv__v">{runtimeLabel(selectedRuntime)}</span>
          </span>
          <span className="kv">
            <span className="kv__k">VIEW</span>
            <span className="kv__v">{view === "history" ? "HIST" : "LIVE"}</span>
          </span>
        </div>
        <div className="statusbar__group statusbar__group--center">
          <span className="kv">
            <span className="kv__k">OK</span>
            <span className="kv__v kv__v--ok">{pad2(stats.ok)}</span>
          </span>
          <span className="kv">
            <span className="kv__k">RUN</span>
            <span className="kv__v kv__v--run">{pad2(stats.run)}</span>
          </span>
          <span className="kv">
            <span className="kv__k">ERR</span>
            <span className="kv__v kv__v--err">{pad2(stats.fail)}</span>
          </span>
          <span className="kv">
            <span className="kv__k">IDL</span>
            <span className="kv__v">{pad2(stats.waiting)}</span>
          </span>
        </div>
        <div className="statusbar__group">
          <span className="kv">
            <span className="kv__k">SIG</span>
            <span className={`kv__v ${sigKvClass}`}>{sig.code}</span>
          </span>
          <span className="kv">
            <span className="kv__k">EV</span>
            <span className="kv__v">{pad4(eventCount)}</span>
          </span>
        </div>
      </footer>
    </div>
  );
}

type LiveViewProps = {
  renderedWorkflows: RenderedWorkflow[];
  stats: StatsSummary;
  logs: LogEntry[];
  logsRef: { current: HTMLDivElement | null };
  onLogsScroll: (e: JSX.TargetedEvent<HTMLDivElement>) => void;
  autoScroll: boolean;
  clockText: string;
};

function LiveView(props: LiveViewProps): JSX.Element {
  const {
    renderedWorkflows,
    stats,
    logs,
    logsRef,
    onLogsScroll,
    autoScroll,
    clockText,
  } = props;
  return (
    <>
      <section className="panel panel--graph">
        <div className="panel__head">
          <div className="panel__title">
            <span className="panel__bracket">[</span>
            <h2>PIPELINE</h2>
            <span className="panel__bracket">]</span>
            <span className="panel__sublabel">topological order</span>
          </div>
          <div className="counts" aria-label="Workflow counts">
            <span className="count count--ok" title="Succeeded">
              <i aria-hidden="true" />
              {pad2(stats.ok)}
            </span>
            <span className="count count--run" title="Running">
              <i aria-hidden="true" />
              {pad2(stats.run)}
            </span>
            <span className="count count--fail" title="Failed">
              <i aria-hidden="true" />
              {pad2(stats.fail)}
            </span>
            <span className="count count--idle" title="Idle / pending">
              <i aria-hidden="true" />
              {pad2(stats.waiting)}
            </span>
          </div>
        </div>

        <div className="graph" role="list">
          {renderedWorkflows.length === 0 ? (
            <div className="empty-card">
              <span className="empty-card__cursor">▌</span>
              <span>NO WORKFLOWS · check workflows.toml</span>
            </div>
          ) : (
            renderedWorkflows.map((wf) => {
              const idx = `W${pad2(wf.index + 1)}`;
              const flag = wf.command ? "RUN" : "BUILD";
              return (
                <article
                  key={wf.name}
                  className={`node node--${wf.status}`}
                  role="listitem"
                >
                  <div className="node__rail">
                    <div className="node__index">{idx}</div>
                    <div className="node__led" aria-hidden="true">
                      <span />
                      <span />
                      <span />
                    </div>
                  </div>
                  <div className="node__body">
                    <div className="node__head">
                      <div className="node__name">{wf.name}</div>
                      <div className="node__flags">
                        <span className={`flag flag--${flag.toLowerCase()}`}>
                          {flag}
                        </span>
                      </div>
                    </div>
                    <div className="node__meta">
                      <span className="meta-key">img</span>
                      <span className="meta-val">{wf.image}</span>
                    </div>
                    <div className="node__meta">
                      <span className="meta-key">dep</span>
                      <span className="meta-val">
                        {wf.depends_on.length === 0 ? (
                          <span className="meta-dim">— none</span>
                        ) : (
                          wf.depends_on.map((d) => (
                            <span key={d} className="dep-pill">
                              <span
                                className="dep-pill__arrow"
                                aria-hidden="true"
                              >
                                ←
                              </span>
                              {d}
                            </span>
                          ))
                        )}
                      </span>
                    </div>
                  </div>
                  <div className="node__status">
                    <span className="node__status-dot" aria-hidden="true" />
                    <span className="node__status-text">
                      {String(wf.status).toUpperCase()}
                    </span>
                  </div>
                </article>
              );
            })
          )}
        </div>
      </section>

      <section className="panel panel--logs">
        <div className="panel__head">
          <div className="panel__title">
            <span className="panel__bracket">[</span>
            <h2>TELEMETRY</h2>
            <span className="panel__bracket">]</span>
            <span className="panel__sublabel">live event stream</span>
          </div>
          <div className="logs__head">
            <span className="kv">
              <span className="kv__k">LINES</span>
              <span className="kv__v">{pad4(logs.length)}</span>
            </span>
            <span className="kv">
              <span className="kv__k">CLK</span>
              <span className="kv__v">{clockText}</span>
            </span>
            <span
              className={`kv kv--toggle ${autoScroll ? "kv--on" : "kv--off"}`}
            >
              <span className="kv__k">FLW</span>
              <span className="kv__v">{autoScroll ? "ON" : "OFF"}</span>
            </span>
          </div>
        </div>

        <div className="logs" ref={logsRef} onScroll={onLogsScroll}>
          {logs.length === 0 ? (
            <div className="logs__empty">
              <span className="logs__empty-cursor">▌</span>
              <span className="logs__empty-text">
                STANDING BY · awaiting pipeline events
              </span>
            </div>
          ) : (
            <ol className="loglines">
              {logs.map((l) => (
                <li key={l.id} className={`logline logline--${l.level}`}>
                  <span className="logline__ts">{formatTimestamp(l.ts)}</span>
                  <span className="logline__sep" aria-hidden="true">
                    │
                  </span>
                  <span className="logline__prefix">{l.prefix}</span>
                  <span className="logline__sep" aria-hidden="true">
                    │
                  </span>
                  <span className="logline__msg">{l.message}</span>
                </li>
              ))}
            </ol>
          )}
        </div>
      </section>
    </>
  );
}

type HistoryViewProps = {
  runs: RunRecord[];
  runsLoading: boolean;
  runsError: string | null;
  selectedRunId: number | null;
  setSelectedRunId: (id: number) => void;
  runEvents: RunEventRecord[];
  runEventsLoading: boolean;
  runEventsError: string | null;
  onRefreshRuns: () => void;
  runEventsRef: { current: HTMLDivElement | null };
};

function HistoryView(props: HistoryViewProps): JSX.Element {
  const {
    runs,
    runsLoading,
    runsError,
    selectedRunId,
    setSelectedRunId,
    runEvents,
    runEventsLoading,
    runEventsError,
    onRefreshRuns,
    runEventsRef,
  } = props;

  const selectedRun = useMemo(
    () => runs.find((r) => r.id === selectedRunId) ?? null,
    [runs, selectedRunId],
  );

  return (
    <>
      <section className="panel panel--runs">
        <div className="panel__head">
          <div className="panel__title">
            <span className="panel__bracket">[</span>
            <h2>RUNS</h2>
            <span className="panel__bracket">]</span>
            <span className="panel__sublabel">recorded pipelines</span>
          </div>
          <div className="logs__head">
            <span className="kv">
              <span className="kv__k">N</span>
              <span className="kv__v">{pad4(runs.length)}</span>
            </span>
            <button
              type="button"
              className="btn btn--ghost"
              onClick={onRefreshRuns}
              disabled={runsLoading}
            >
              {runsLoading ? "LOADING…" : "REFRESH"}
            </button>
          </div>
        </div>

        <div className="runs" role="list">
          {runsError ? (
            <div className="empty-card">
              <span className="empty-card__cursor">!</span>
              <span>{runsError}</span>
            </div>
          ) : runs.length === 0 && !runsLoading ? (
            <div className="empty-card">
              <span className="empty-card__cursor">▌</span>
              <span>NO RUNS · trigger a build or run</span>
            </div>
          ) : (
            runs.map((run) => {
              const isActive = run.id === selectedRunId;
              const target = run.workflow ?? "[ALL]";
              return (
                <button
                  type="button"
                  key={run.id}
                  className={`run-row${isActive ? " run-row--active" : ""}`}
                  onClick={() => setSelectedRunId(run.id)}
                  role="listitem"
                >
                  <div className="run-row__rail">
                    <div className="run-row__id">#{run.id}</div>
                    <div className={`run-row__mode mode--${run.mode}`}>
                      {run.mode.toUpperCase()}
                    </div>
                  </div>
                  <div className="run-row__body">
                    <div className="run-row__head">
                      <span className="run-row__target">{target}</span>
                      <span className={`run-pill ${runStatusClass(run.status)}`}>
                        {run.status.toUpperCase()}
                      </span>
                    </div>
                    <div className="run-row__meta">
                      <span className="meta-key">when</span>
                      <span className="meta-val">
                        {formatRunStarted(run.started_at)}
                      </span>
                    </div>
                    <div className="run-row__meta">
                      <span className="meta-key">dur</span>
                      <span className="meta-val">
                        {formatDuration(run.started_at, run.finished_at)}
                      </span>
                    </div>
                  </div>
                </button>
              );
            })
          )}
        </div>
      </section>

      <section className="panel panel--logs">
        <div className="panel__head">
          <div className="panel__title">
            <span className="panel__bracket">[</span>
            <h2>EVENTS</h2>
            <span className="panel__bracket">]</span>
            <span className="panel__sublabel">
              {selectedRun
                ? `run #${selectedRun.id} · ${selectedRun.mode}`
                : "select a run"}
            </span>
          </div>
          <div className="logs__head">
            <span className="kv">
              <span className="kv__k">LINES</span>
              <span className="kv__v">{pad4(runEvents.length)}</span>
            </span>
            {selectedRun && (
              <span className="kv">
                <span className="kv__k">DUR</span>
                <span className="kv__v">
                  {formatDuration(
                    selectedRun.started_at,
                    selectedRun.finished_at,
                  )}
                </span>
              </span>
            )}
          </div>
        </div>

        <div className="logs" ref={runEventsRef}>
          {runEventsError ? (
            <div className="logs__empty">
              <span className="logs__empty-cursor">!</span>
              <span className="logs__empty-text">{runEventsError}</span>
            </div>
          ) : runEventsLoading ? (
            <div className="logs__empty">
              <span className="logs__empty-cursor">▌</span>
              <span className="logs__empty-text">LOADING EVENTS…</span>
            </div>
          ) : selectedRunId == null ? (
            <div className="logs__empty">
              <span className="logs__empty-cursor">▌</span>
              <span className="logs__empty-text">SELECT A RUN</span>
            </div>
          ) : runEvents.length === 0 ? (
            <div className="logs__empty">
              <span className="logs__empty-cursor">▌</span>
              <span className="logs__empty-text">NO EVENTS RECORDED</span>
            </div>
          ) : (
            <ol className="loglines">
              {runEvents.map((event) => {
                const level = levelForRunEvent(event);
                const prefix =
                  event.kind === "pipeline-started" ||
                  event.kind === "pipeline-finished" ||
                  event.kind === "pipeline-cancelled"
                    ? "PIPELINE"
                    : event.kind === "error"
                      ? "ERROR"
                      : buildPrefix(event.phase, event.workflow);
                return (
                  <li
                    key={event.id}
                    className={`logline logline--${level}`}
                  >
                    <span className="logline__ts">
                      {formatTimestamp(event.timestamp_ms)}
                    </span>
                    <span className="logline__sep" aria-hidden="true">
                      │
                    </span>
                    <span className="logline__prefix">{prefix}</span>
                    <span className="logline__sep" aria-hidden="true">
                      │
                    </span>
                    <span className="logline__msg">{event.message}</span>
                  </li>
                );
              })}
            </ol>
          )}
        </div>
      </section>
    </>
  );
}
