# Reconciliation Model — Desired / Observed / Effective

## The Problem

The old `WorkerStatus` enum mixed three concerns into one value:

```mermaid
graph LR
    subgraph "OLD: WorkerStatus (8 variants)"
        A[Starting] --> B[Active]
        B --> C[Unresponsive]
        C --> D[Stale]
        B --> E[Paused]
        B --> F[Stopping]
        F --> G[Stopped]
        B --> H["Error(msg)"]
    end
    style A fill:#ccc
    style B fill:#4a4
    style C fill:#cc4
    style D fill:#c44
    style E fill:#cc4
    style F fill:#cc4
    style G fill:#888
    style H fill:#c44
```

Every CLI command had its own ad-hoc reconciliation between this persisted
status and tmux reality. Bugs were inevitable.

## The Solution: Three Orthogonal Axes

```mermaid
graph TD
    subgraph "1. DESIRED (persisted in fleet.json)"
        D1[Running]
        D2[Paused]
        D3[Stopped]
    end

    subgraph "2. OBSERVED (computed fresh, never stored)"
        O1["TransportState<br/>Alive | Dead | Unknown"]
        O2["SessionStatus<br/>idle | working | blocked | loading"]
        O3["CognitiveState<br/>Fresh(status) | Stale | None"]
    end

    subgraph "3. EFFECTIVE (output of reconcile)"
        E1[Healthy]
        E2[Diverged]
        E3[Suspect]
        E4[Blocked]
        E5[Paused]
        E6[Stopped]
        E7["Error(msg)"]
    end

    D1 & D2 & D3 --> R["reconcile()"]
    O1 & O2 & O3 --> R
    R --> E1 & E2 & E3 & E4 & E5 & E6 & E7

    style D1 fill:#4a4,color:#fff
    style D2 fill:#cc4
    style D3 fill:#888,color:#fff
    style E1 fill:#4a4,color:#fff
    style E2 fill:#c44,color:#fff
    style E3 fill:#cc4
    style E4 fill:#c44,color:#fff
    style E5 fill:#cc4
    style E6 fill:#888,color:#fff
    style E7 fill:#c44,color:#fff
    style R fill:#46c,color:#fff
```

| Axis | Source | Persisted? | Purpose |
|------|--------|------------|---------|
| **Desired** | Operator (deploy, kill, freeze) | Yes (fleet.json) | What you *want* |
| **Observed** | Transport + cognitive probes | No (fresh each time) | What *reality* says |
| **Effective** | `reconcile(desired, observed)` | No (computed) | What you *see* |

## How `cs ensemble` Builds Each Column

```mermaid
sequenceDiagram
    participant E as cs ensemble
    participant F as fleet.json
    participant T as tmux (transport)
    participant C as cognitive/*.json
    participant R as reconcile()

    E->>F: load fleet
    Note over F: desired = Running<br/>restart_count = 0

    loop for each worker
        E->>T: is_alive(worker)?
        T-->>E: true (session exists)
        E->>T: detect_status(worker)
        T-->>E: SessionStatus::Ready
        Note over E: transport = Alive<br/>session = "idle"

        E->>C: read cognitive/{worker}.json
        C-->>E: not found
        Note over E: cognitive = None

        E->>R: reconcile(Running, {Alive, "idle", None}, 0, 3)
        R-->>E: (Healthy, [Noop])
    end

    Note over E: Display:<br/>DESIRED=running  EFFECTIVE=healthy  LIVE=idle
```

## The `reconcile()` Decision Matrix

```mermaid
graph TD
    Start["reconcile(desired, observed, failures, max)"] --> D{desired?}

    D -->|Running| T1{transport?}
    D -->|Paused| T2{transport?}
    D -->|Stopped| T3{transport?}

    T1 -->|Alive| S{"session?"}
    T1 -->|Dead| CB{"failures >= max?"}
    T1 -->|Unknown| SUS1["Suspect + Noop"]

    S -->|"blocked / trust-prompt"| BLK["Blocked + Noop"]
    S -->|other| COG{"cognitive?"}

    COG -->|"Fresh / None"| H["Healthy + Noop"]
    COG -->|Stale| SUS2["Suspect + RecordFailure"]

    CB -->|Yes| ERR["Error + CircuitBreak"]
    CB -->|No| DIV1["Diverged + Respawn"]

    T2 -->|Alive| P1["Paused + Freeze"]
    T2 -->|"Dead / Unknown"| P2["Paused + Noop"]

    T3 -->|Alive| DIV2["Diverged + Kill"]
    T3 -->|"Dead / Unknown"| STP["Stopped + Noop"]

    style H fill:#4a4,color:#fff
    style DIV1 fill:#c44,color:#fff
    style DIV2 fill:#c44,color:#fff
    style SUS1 fill:#cc4
    style SUS2 fill:#cc4
    style BLK fill:#c44,color:#fff
    style ERR fill:#c44,color:#fff
    style P1 fill:#cc4
    style P2 fill:#cc4
    style STP fill:#888,color:#fff
    style Start fill:#46c,color:#fff
```

## Why Your Workers Show "idle"

```mermaid
graph LR
    TMX["tmux session alive<br/>(process running)"] -->|is_alive = true| TA[TransportState::Alive]
    TMX -->|"capture last 30 lines<br/>sees ❯ prompt"| SS["SessionStatus::Ready<br/>(displayed as 'idle')"]

    TA --> OBS["ObservedState"]
    SS --> OBS
    NC["No cognitive/*.json file"] -->|"cognitive = None"| OBS

    DES["desired = Running<br/>(from fleet.json)"] --> REC["reconcile()"]
    OBS --> REC

    REC --> EFF["Effective = Healthy"]
    REC --> ACT["Action = Noop"]

    style TA fill:#4a4,color:#fff
    style SS fill:#888,color:#fff
    style EFF fill:#4a4,color:#fff
    style ACT fill:#888,color:#fff
    style REC fill:#46c,color:#fff
```

**idle** means the tmux session is alive and showing the input prompt (`❯`).
The agent is ready but has no work assigned. This is normal when:

- No molecule is dispatched to the worker
- The worker finished its previous task
- The fleet was deployed but never given instructions

The effective status is **Healthy** because reality matches intent:
you asked for `Running`, the process is alive, and nothing is wrong.

## Common Scenarios

| Scenario | Desired | Transport | Session | Cognitive | Effective | Live |
|----------|---------|-----------|---------|-----------|-----------|------|
| Agent working normally | Running | Alive | working | Fresh("working") | Healthy | working |
| Agent idle, waiting for work | Running | Alive | idle | None | Healthy | idle |
| Agent crashed | Running | Dead | - | - | Diverged | - |
| Agent frozen by operator | Paused | Dead | - | - | Paused | - |
| Agent killed, session gone | Stopped | Dead | - | - | Stopped | - |
| Zombie (killed but tmux lingers) | Stopped | Alive | idle | - | Diverged | idle |
| Agent stuck on permission prompt | Running | Alive | blocked | - | Blocked | blocked |
| Restart limit hit | Running | Dead | - | - | Error | - |
| Agent alive but stale cognitive | Running | Alive | idle | Stale | Suspect | idle |
