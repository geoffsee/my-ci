import { useEffect, useMemo, useState } from "preact/hooks";
import type { JSX } from "preact";

const EMPTY_LOG_TEXT = "Waiting for pipeline output...";

type Workflow = {
  name: string;
  image: string;
  command: string[] | null;
  depends_on: string[];
};

type RenderedWorkflow = Workflow & {
  status: string;
  meta: string;
};

type PipelineResponse = {
  name: string;
  workflows: Workflow[];
};

type CommandResponse = {
  message: string;
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

export default function App(): JSX.Element {
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [statuses, setStatuses] = useState<Map<string, string>>(() => new Map());
  const [running, setRunning] = useState(false);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [logOutput, setLogOutput] = useState<string>(EMPTY_LOG_TEXT);
  const [connectionState, setConnectionState] =
    useState<ConnectionState>("Connecting...");
  const [clock, setClock] = useState(() => new Date().toLocaleTimeString());
  const [projectName, setProjectName] = useState("my-ci");

  const statusText = running ? "Running" : "Idle";

  const renderedWorkflows = useMemo<RenderedWorkflow[]>(
    () =>
      workflows.map((wf) => {
        const status = statuses.get(wf.name) || "idle";
        const deps = wf.depends_on.length ? `deps: ${wf.depends_on.join(", ")}` : "no deps";
        const meta = `${wf.image} | ${deps}${wf.command ? " | runnable" : ""}`;
        return { ...wf, status, meta };
      }),
    [workflows, statuses]
  );

  useEffect(() => {
    const intervalId = window.setInterval(() => {
      setClock(new Date().toLocaleTimeString());
    }, 1000);
    return () => window.clearInterval(intervalId);
  }, []);

  useEffect(() => {
    loadPipeline();
    const source = new EventSource("/api/events");

    source.addEventListener("open", () => {
      setConnectionState("Live");
    });

    source.addEventListener("error", () => {
      setConnectionState("Reconnecting...");
    });

    source.addEventListener("pipeline", (msg) => {
      const event = JSON.parse((msg as MessageEvent<string>).data) as PipelineEvent;
      const kind = event.kind;

      if (kind === "pipeline-started") {
        setRunning(true);
        appendLog(`[pipeline] ${event.message}`);
        return;
      }

      if (kind === "pipeline-finished" || kind === "pipeline-cancelled") {
        setRunning(false);
        appendLog(`[pipeline] ${event.message}`);
        return;
      }

      if (kind === "workflow-status") {
        if (event.workflow && event.status) {
          const wfName = event.workflow;
          const wfStatus = event.status;
          setStatuses((prev) => {
            const next = new Map(prev);
            next.set(wfName, wfStatus);
            return next;
          });
        }
        appendLog(`[${event.phase}:${event.workflow}] ${event.message}`);
        return;
      }

      if (kind === "log") {
        appendLog(`[${event.phase}:${event.workflow}] ${event.message}`);
        return;
      }

      if (kind === "error") {
        setRunning(false);
        if (event.workflow && event.status) {
          const wfName = event.workflow;
          const wfStatus = event.status;
          setStatuses((prev) => {
            const next = new Map(prev);
            next.set(wfName, wfStatus);
            return next;
          });
        }
        appendLog(`[error] ${event.message}`);
      }
    });

    return () => source.close();
  }, []);

  async function loadPipeline(): Promise<void> {
    const res = await fetch("/api/pipeline");
    const data = (await res.json()) as PipelineResponse;
    setWorkflows(data.workflows);
    setProjectName(data.name);
  }

  function appendLog(message: string): void {
    setLogOutput((prev) => {
      const base = prev === EMPTY_LOG_TEXT ? "" : prev;
      return `${base}${message.endsWith("\n") ? message : `${message}\n`}`;
    });
  }

  async function command(path: string): Promise<void> {
    const workflow = selectedWorkflow || null;
    const res = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ workflow })
    });
    const data = (await res.json()) as CommandResponse;
    appendLog(`[control] ${data.message}`);
  }

  function clearLogs(): void {
    setLogOutput(EMPTY_LOG_TEXT);
  }

  return (
    <>
      <header>
        <div>
          <h1 id="title">{projectName} Pipeline</h1>
          <div className="sub" id="connection">{connectionState}</div>
        </div>
        <div className="controls">
          <select
            id="workflow"
            value={selectedWorkflow}
            onChange={(event) =>
              setSelectedWorkflow((event.currentTarget as HTMLSelectElement).value)
            }
          >
            <option value="">All workflows</option>
            {workflows.map((wf) => (
              <option key={wf.name} value={wf.name}>
                {wf.name}
              </option>
            ))}
          </select>
          <button
            id="build"
            className="primary"
            title="Build selected workflow"
            disabled={running}
            onClick={() => command("/api/build")}
          >
            Build
          </button>
          <button
            id="run"
            className="primary"
            title="Run selected workflow"
            disabled={running}
            onClick={() => command("/api/run")}
          >
            Run
          </button>
          <button
            id="stop"
            className="danger"
            title="Stop the active pipeline"
            disabled={!running}
            onClick={() => command("/api/stop")}
          >
            Stop
          </button>
          <button id="clear" title="Clear log output" onClick={clearLogs}>
            Clear
          </button>
        </div>
      </header>
      <main>
        <section>
          <div className="toolbar">
            <h2>Pipeline</h2>
            <div className="status" id="status">{statusText}</div>
          </div>
          <div className="graph" id="graph">
            {renderedWorkflows.map((wf) => (
              <div className={`node ${wf.status}`} key={wf.name}>
                <div className="dot"></div>
                <div>
                  <div className="name">{wf.name}</div>
                  <div className="meta">{wf.meta}</div>
                </div>
                <div className="badge">{wf.status}</div>
              </div>
            ))}
          </div>
        </section>
        <section>
          <div className="toolbar">
            <h2>Logs</h2>
            <div className="status" id="clock">{clock}</div>
          </div>
          <div className="logs" id="logs">
            {logOutput === EMPTY_LOG_TEXT ? (
              <span className="empty">{EMPTY_LOG_TEXT}</span>
            ) : (
              logOutput
            )}
          </div>
        </section>
      </main>
    </>
  );
}
