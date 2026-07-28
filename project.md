# Product Requirements Document

## Unified Windows System Manager, Performance Controller, Historical Monitor, and Diagnostic Assistant

**Working title:** Atlas
**Document status:** Initial full product definition
**Platform:** Windows 11 first, with architecture designed for future Windows versions
**Product category:** System monitoring, process management, performance optimization, diagnostics, privacy visibility, startup management, service management, and troubleshooting
**Primary audience:** General users, gamers, developers, IT professionals, support engineers, system administrators, security-conscious users, and power users

> This is a requirements document, not a claim that every described capability is implemented. See [docs/current-state.md](docs/current-state.md) for the current as-built baseline and [docs/phases.md](docs/phases.md) for itemized status.

---

# 1. Product Summary

Atlas is a modern replacement for Windows Task Manager, Process Explorer, System Informer, Process Lasso, Task Manager DeLuxe, Resource Monitor, Startup Apps, Reliability Monitor, selected Event Viewer workflows, and historical monitoring tools such as AppControl.

The product combines:

* Real-time process monitoring.
* Historical system activity.
* Deep process inspection.
* CPU and performance policy management.
* Application, service, startup, and session management.
* Hardware resource monitoring.
* Privacy activity monitoring.
* Incident diagnosis.
* Safe corrective actions.
* Natural-language system analysis.
* A modern interface that remains usable for non-technical users.
* Low background resource consumption.
* Fast startup and immediate response.
* Local-first data storage and privacy.

The product should not merely display technical measurements. It should answer practical user questions:

* Why is my computer slow?
* What caused the frame drop five minutes ago?
* Which application is using most of my memory?
* What changed since yesterday?
* Why did my fans become loud?
* Which process is preventing a file from being deleted?
* Which application used my microphone?
* What caused my battery to drain?
* Why does one application keep starting automatically?
* Is this process safe to stop?
* What will happen if I disable this service?
* Which background applications are affecting my game?
* Did a Windows update or application update cause the issue?
* What should I change, and can I reverse the change?

The product should bridge the gap between raw system data and safe user decisions.

---

# 2. Product Vision

Create the default control center for understanding and managing a Windows computer.

The product should offer the depth expected by advanced users without forcing general users to understand internal Windows terminology.

The long-term goal is to create a unified system intelligence layer that can:

1. Observe the system.
2. Record relevant activity.
3. Detect abnormal behavior.
4. Correlate related events.
5. Explain likely causes.
6. Show supporting evidence.
7. recommend safe actions.
8. apply approved actions.
9. measure whether the action solved the issue.
10. reverse the action when necessary.

The product should replace fragmented troubleshooting workflows that currently require multiple disconnected tools.

---

# 3. Core Product Principles

## 3.1 Explain before acting

The application must explain what a process, service, startup entry, driver, scheduled task, or system setting does before offering destructive or disruptive controls.

Every action should answer:

* What is this?
* Why is it running?
* Is it part of Windows?
* Is it part of an installed application?
* What is currently using it?
* Is stopping it likely to cause data loss?
* Will it restart automatically?
* Is the change temporary or permanent?
* How can the change be reversed?

## 3.2 Evidence before conclusions

The application must distinguish between:

* Confirmed facts.
* Strong correlations.
* Possible causes.
* Low-confidence hypotheses.

For example:

> A frame-rate drop occurred at 9:17:12 PM. During the same 14-second period, memory usage reached 96%, EpicWebHelper generated sustained disk activity, and Windows memory compression increased. Memory pressure probably contributed to the slowdown. EpicWebHelper may have been a contributing process, but the data does not prove it was the only cause.

The application must avoid presenting temporal correlation as confirmed causation.

## 3.3 Safe by default

The product should prefer reversible operations.

Examples:

* Suspend before terminate when appropriate.
* Temporarily disable before permanently deleting.
* Create restore points before sensitive configuration changes.
* Save previous priority, affinity, startup, and service settings.
* Offer one-click rollback.
* Warn when unsaved user data may be lost.
* Block or strongly warn against stopping critical Windows processes.

## 3.4 Progressive disclosure

The interface should support three information levels:

### Simple level

For general users:

* Clear process names.
* Application icons.
* Plain-language explanations.
* Health indicators.
* Recommended actions.
* Minimal technical terminology.

### Detailed level

For informed users:

* CPU, memory, disk, network, GPU, energy, and historical charts.
* Process relationships.
* Startup behavior.
* Service dependencies.
* Signatures.
* File paths.
* Active network destinations.

### Expert level

For power users and administrators:

* Process identifiers.
* Parent and child process relationships.
* Threads.
* Handles.
* Modules.
* Dynamic-link libraries.
* Tokens and privileges.
* Integrity levels.
* Command-line arguments.
* Environment variables.
* Processor affinity.
* Input/output priority.
* Working sets.
* Page faults.
* Graphics processing unit engine activity.
* Service host grouping.
* Driver details.
* Stack information where technically and legally feasible.

## 3.5 Low overhead

The monitoring tool must not become a meaningful source of system load.

The product should minimize:

* Processor use.
* Graphics processing unit use.
* Memory use.
* Disk writes.
* Battery impact.
* Wake-ups.
* Background network use.
* User-interface rendering cost.

## 3.6 Local-first privacy

System activity data should remain on the user’s device unless the user explicitly chooses otherwise.

No account should be required for core functionality.

No system history should be uploaded by default.

The product should not host or upload to any artificial-intelligence model. In-app natural-language analysis is deterministic and fully local. Integration with an external model is provided only through an optional, off-by-default, read-only MCP server; when a user enables it and their MCP client calls a tool, the product must clearly disclose that the returned data leaves the device for that client's model provider, and must apply redaction to that data by default.

## 3.7 Fast interaction

The application should feel immediate even when the computer is under pressure.

Critical controls such as opening the application, searching for a process, inspecting a resource spike, or terminating an unresponsive application must remain responsive during high processor or memory utilization.

---

# 4. Problem Definition

Windows includes multiple tools that partially address system monitoring and management:

* Task Manager.
* Resource Monitor.
* Performance Monitor.
* Reliability Monitor.
* Event Viewer.
* Services.
* Startup Apps.
* System Configuration.
* Windows Security.
* Device Manager.
* Task Scheduler.

Advanced users often add:

* Process Explorer.
* System Informer.
* Process Lasso.
* Task Manager DeLuxe.
* Autoruns.
* Hardware monitoring tools.
* Network monitoring tools.
* Historical activity tools.

These tools have several recurring weaknesses.

## 4.1 Fragmentation

Users must open several applications to understand one issue.

A gaming slowdown might require:

* Task Manager for processes.
* Resource Monitor for disk activity.
* Hardware monitoring software for temperature.
* Reliability Monitor for crashes.
* Event Viewer for system errors.
* Startup Apps for background programs.
* Process Explorer for process relationships.
* Process Lasso for processor controls.

The tools do not provide a unified explanation.

## 4.2 Current-state bias

Most tools show what is happening now.

Many performance problems disappear before the user opens the monitoring tool.

The user cannot easily inspect:

* A processor spike from five minutes ago.
* An application that briefly accessed the microphone.
* A background update that saturated the disk.
* A process that started and exited.
* A service that restarted.
* A thermal event that occurred during gameplay.
* A battery-drain event while the computer was idle.

## 4.3 Data without interpretation

Existing tools show measurements but rarely explain:

* Whether the value is normal.
* Whether the issue is temporary.
* Which applications collectively caused the pressure.
* Whether closing an application is safe.
* Whether the process is a main application, helper process, service, browser tab, extension, renderer, or system component.
* Whether the issue requires action.

## 4.4 Technical naming

Users see labels such as:

* Service Host.
* Runtime Broker.
* Desktop Window Manager.
* Client Server Runtime Process.
* Antimalware Service Executable.
* WebView helper.
* Search host.
* State Repository Service.
* Distributed Component Object Model Server Process Launcher.

The tools provide little plain-language context.

## 4.5 Weak historical context

Existing historical tools often lack one or more of the following:

* Per-process detail.
* Second-level precision.
* Privacy events.
* Application update events.
* Service changes.
* Temperature correlation.
* Network activity correlation.
* Clear timelines.
* Evidence-based diagnosis.
* Corrective-action workflows.

## 4.6 Overwhelming expert tools

Process Explorer, System Informer, and similar tools expose useful technical details but can overwhelm less technical users.

The problem is not that they provide too much information. The problem is that they present most information at the same visual priority.

## 4.7 Weak performance control

Windows Task Manager allows temporary priority and processor-affinity changes, but it is not designed for persistent performance policies.

Users need rules such as:

* Always run a game at high performance.
* Keep background launchers at low priority.
* Restrict an application to efficiency cores.
* Change the power mode when a selected application opens.
* Return to the previous power mode after the application closes.
* Limit the effect of a process that repeatedly saturates the processor.
* Apply different policies on battery and external power.

## 4.8 Unsafe actions

The existing tools often expose actions such as End Task without sufficient explanation.

Users can accidentally:

* Lose unsaved work.
* Stop a required service.
* destabilize an application.
* break networking.
* disable security components.
* create persistent configuration problems.

## 4.9 No outcome verification

Existing tools rarely verify whether a recommended or user-triggered action solved the original problem.

A complete troubleshooting product should compare:

* Before the change.
* During the change.
* After the change.

---

# 5. Product Goals

## 5.1 Primary goals

1. Replace the most common Windows process and performance management tools with one coherent application.
2. Provide both real-time and historical system visibility.
3. Offer deeper process inspection than Windows Task Manager.
4. Offer persistent performance controls similar to Process Lasso.
5. Present system-wide information similar to Task Manager DeLuxe without inheriting its interface complexity.
6. Support investigation workflows similar to Process Explorer and System Informer.
7. provide timeline-based diagnostics similar to AppControl.
8. Explain technical information in understandable language.
9. Provide safe, reversible corrective actions.
10. Operate with lower resource consumption than comparable monitoring applications.
11. Start quickly and remain responsive under system stress.
12. Provide a modern and visually coherent Windows 11 interface.
13. Respect privacy through local processing and explicit permission boundaries.
14. Provide deterministic in-app natural-language analysis without making unsupported claims, and expose grounded, citation-ready evidence to the user's own AI client through an optional read-only MCP server.
15. Serve general users and experts through progressive disclosure rather than separate products.

## 5.2 Secondary goals

1. Reduce the time required to identify common Windows performance problems.
2. Reduce accidental system damage caused by uninformed process termination or service changes.
3. Help users understand background activity.
4. Help gamers identify frame-time disruptions.
5. Help developers inspect processes, ports, files, and service dependencies.
6. Help IT staff diagnose devices without installing multiple utilities.
7. Help users understand battery and thermal behavior.
8. Create a foundation for future remote support and fleet management products.

---

# 6. Non-Goals for the Initial Product

The initial version should not attempt to become:

* A full antivirus replacement.
* A full endpoint detection and response platform.
* A full packet inspection suite.
* A complete registry editor.
* A complete driver development or kernel debugging environment.
* A full enterprise mobile device management platform.
* A replacement for Windows Update.
* A hardware overclocking utility.
* A general file manager.
* A generic computer cleaning product that deletes files without clear evidence.
* A software-uninstallation marketplace.
* A product that applies aggressive performance modifications automatically without user approval.

The architecture may allow selected future integrations, but these areas should not distort the initial product.

---

# 7. Target Users

## 7.1 General Windows user

Needs:

* Understand why the computer is slow.
* Close frozen applications safely.
* Reduce unnecessary startup applications.
* Understand high memory use.
* See which applications use the camera or microphone.
* Receive clear recommendations.
* Avoid technical language.

## 7.2 Gamer

Needs:

* Identify processor, graphics processor, memory, disk, thermal, and background activity during frame drops.
* Compare gameplay sessions.
* Detect launcher, overlay, update, recording, browser, and antivirus interference.
* Apply game-specific performance profiles.
* Manage processor affinity and power modes.
* Detect thermal throttling.
* See frame-time-related system events where supported.

## 7.3 Developer

Needs:

* Inspect process trees.
* Find ports and owning processes.
* Find file locks.
* Inspect command lines, environment variables, threads, modules, handles, and network endpoints.
* Suspend and resume processes.
* Track resource behavior during builds, tests, and local servers.
* Compare application versions and resource regressions.

## 7.4 Information technology support specialist

Needs:

* Quickly inspect device health.
* Understand startup delays.
* Identify failing services.
* Review recent changes.
* Export diagnostic reports.
* Collect evidence without exposing unnecessary private data.
* Apply reversible fixes.
* Compare before-and-after behavior.

## 7.5 System administrator

Needs:

* Deep process and service visibility.
* Persistent performance rules.
* Session and user management.
* Scheduled task visibility.
* Driver and service relationships.
* Signed and unsigned binary analysis.
* Exportable structured data.
* Command-line and scripting access.

## 7.6 Security-conscious user

Needs:

* Detect camera, microphone, and location usage.
* Identify unsigned or newly introduced executables.
* Review application network activity.
* Inspect signatures and publishers.
* Track service installation and changes.
* Understand startup persistence mechanisms.
* See what changed while the system was idle.

---

# 8. User Experience Architecture

The application should be organized around user questions rather than internal Windows subsystems.

Primary navigation:

1. Overview.
2. Live Activity.
3. Timeline.
4. Applications.
5. Processes.
6. Performance.
7. Rules and Optimization.
8. Startup and Background.
9. Services and Tasks.
10. Privacy.
11. Network.
12. System Changes.
13. Diagnostics.
14. Reports.
15. Settings.

Expert tools can be exposed through contextual panels rather than forcing users to navigate an entirely separate application.

---

# 9. Feature Requirements

# 9.1 Overview Dashboard

The Overview page should provide a clear summary of current system condition.

It should display:

* Overall system health status.
* Current processor utilization.
* Current memory utilization.
* Current graphics processor utilization.
* Current disk utilization.
* Current network activity.
* Current device temperature where sensors are available.
* Battery status and estimated discharge behavior.
* Number of active applications.
* Number of background processes.
* Number of active services.
* Recent warnings.
* Recent abnormal events.
* Top resource-consuming applications.
* Most recent significant system change.
* Active performance profile.
* Active privacy-sensitive devices.
* Whether a diagnostic incident is currently being recorded.

The overall health state should not rely on one arbitrary score. It should show separate dimensions:

* Responsiveness.
* Resource pressure.
* Thermal condition.
* Battery condition.
* Storage condition.
* Startup condition.
* Background activity.
* Privacy activity.
* Stability.
* Security signals.

Every health indicator must be expandable to show evidence.

Example:

> Memory pressure is high.
> 84% of physical memory is in use. Brave Browser, Rocket League, Discord, Visual Studio Code, and background application helpers account for most active memory. Windows has started compressing memory, but page-file pressure is currently low.

The interface should avoid vague labels such as “Your computer needs attention” without evidence.

---

# 9.2 Live Activity

The Live Activity page should replace the basic Windows Task Manager Processes view.

## 9.2.1 Application-first grouping

Processes should be grouped under recognizable applications.

Examples:

* Brave Browser.
* Discord.
* Visual Studio Code.
* Epic Games Launcher.
* Rocket League.
* Windows Search.
* Windows Security.

The application should identify:

* Main application process.
* Renderer processes.
* Graphics processor processes.
* Extension processes.
* Browser tabs where technically available.
* Helper processes.
* Crash handlers.
* Updaters.
* Services.
* Background agents.

The application should avoid showing “Brave Browser (32)” without explaining what the 32 processes represent.

## 9.2.2 Resource columns

The default table should support:

* Processor use.
* Memory use.
* Graphics processor use.
* Graphics memory use.
* Disk read.
* Disk write.
* Network upload.
* Network download.
* Energy use.
* Power impact.
* Temperature contribution where estimable.
* Process status.
* Efficiency state.
* Responsiveness.
* Start time.
* User.
* Publisher.
* Trust status.

Expert columns should include:

* Process identifier.
* Parent process identifier.
* Thread count.
* Handle count.
* Commit size.
* Working set.
* Private working set.
* Shared memory.
* Page faults.
* Input/output operations.
* Kernel time.
* User time.
* Processor cycle count.
* Base priority.
* Dynamic priority.
* Integrity level.
* Architecture.
* Processor affinity.
* Input/output priority.
* Memory priority.
* Graphics engine.
* Command line.
* File path.
* Package identity.
* Service association.

## 9.2.3 Smart sorting

Users should be able to sort by:

* Current use.
* Average use.
* Peak use.
* Change during the last minute.
* Historical impact.
* Startup impact.
* Energy impact.
* Abnormality.
* User attention required.

Example:

A process that briefly used 90% of the processor but is currently at 0% should still be discoverable through “Recent peak.”

## 9.2.4 Search

The global search field should support:

* Application name.
* Process name.
* Publisher.
* File path.
* Process identifier.
* Service name.
* Port number.
* Domain.
* Internet Protocol address.
* Window title.
* Command-line argument.
* Module name.
* Driver name.
* Scheduled task name.

Search results should combine live and historical matches.

## 9.2.5 Status interpretation

Instead of only showing numeric status, the application should identify:

* Running normally.
* Waiting.
* Suspended.
* Not responding.
* Starting.
* Stopping.
* Restarting.
* Sleeping.
* Efficiency-limited.
* Processor-limited.
* Disk-bound.
* Memory-pressured.
* Network-bound.
* Graphics processor-bound.
* Waiting for another process.
* Blocked by file input/output.
* Terminating.
* Recently crashed.

## 9.2.6 Contextual actions

Available actions should include:

* Open application.
* Bring window to front.
* Minimize.
* Close normally.
* Request graceful shutdown.
* Suspend.
* Resume.
* End process.
* End process tree.
* Restart process.
* Restart application.
* Open file location.
* Open installation location.
* View file properties.
* Copy file path.
* Copy command line.
* Search history.
* Inspect process.
* View network connections.
* View open files.
* View loaded modules.
* View threads.
* View services.
* Create performance rule.
* Add alert.
* Exclude from monitoring.
* Submit file hash to configured security service with explicit user permission.
* Verify digital signature.
* Compare with previous versions.

Actions must adapt to process type.

For example, “Close normally” should be preferred for desktop applications, while “Restart service” should be offered for service-hosted processes.

---

# 9.3 Historical Timeline

The Timeline is a central product feature.

## 9.3.1 Retention

Default local retention should be configurable.

Recommended options:

* 24 hours.
* 72 hours.
* 7 days.
* 14 days.
* 30 days.
* Custom retention.
* Storage-size-based retention.

The product should explain storage implications.

Older high-frequency data may be downsampled while preserving significant events and peaks.

## 9.3.2 Time resolution

The system should support:

* One-second precision for recent activity.
* Higher precision for selected incident recording where technically feasible.
* Aggregated minute-level data for longer retention.
* Event-level precision for application launches, process exits, service changes, privacy access, crashes, and configuration changes.

## 9.3.3 Timeline tracks

The timeline should support synchronized tracks for:

* Processor use.
* Per-core processor use.
* Memory use.
* Memory compression.
* Paging activity.
* Disk activity.
* Disk latency.
* Network activity.
* Graphics processor use.
* Graphics memory use.
* Temperature.
* Fan speed where available.
* Power usage.
* Battery discharge.
* Application launches.
* Process launches.
* Process exits.
* Crashes.
* Unresponsive states.
* Service starts.
* Service stops.
* Service restarts.
* Scheduled task execution.
* Application installation.
* Application updates.
* Driver installation.
* Windows updates.
* Startup registration changes.
* Camera use.
* Microphone use.
* Location use.
* Screen capture activity where detectable.
* Power mode changes.
* Sleep and wake events.
* User login.
* User logout.
* Lock and unlock.
* Network changes.
* External device connection.
* Thermal throttling.
* Performance-rule activation.
* User actions performed through the application.

## 9.3.4 Event inspection

Selecting a point or region on the timeline should show:

* Processes active during the selected period.
* Top resource consumers.
* New processes that started.
* Processes that exited.
* Services that changed.
* Applications that updated.
* Network destinations contacted.
* Privacy-sensitive devices used.
* Thermal changes.
* Power changes.
* Related warnings.
* Suspected correlations.

## 9.3.5 Comparison mode

Users should be able to compare:

* Two time periods.
* Two gameplay sessions.
* Before and after a software update.
* Before and after changing a performance rule.
* Battery behavior on different days.
* Startup performance across multiple boots.
* Resource use between two application versions.

Comparison should include:

* Average use.
* Peak use.
* Duration above threshold.
* New background processes.
* Removed background processes.
* Startup-time changes.
* Crash changes.
* Temperature differences.
* Battery differences.
* Network differences.

## 9.3.6 Bookmarking incidents

Users should be able to mark:

* “The computer froze here.”
* “Frame rate dropped here.”
* “The fan became loud here.”
* “The microphone indicator appeared here.”
* “The battery started draining here.”
* “The build became slow here.”

A keyboard shortcut should create an incident marker without requiring the application to be in focus.

## 9.3.7 Automatic incident detection

The product should detect potential incidents such as:

* Sustained processor saturation.
* High disk latency.
* Sudden memory pressure.
* Heavy paging.
* Graphics processor saturation.
* Graphics memory exhaustion.
* Thermal throttling.
* Rapid battery discharge.
* Process crash.
* Application hang.
* Service failure.
* Repeated process restart.
* Excessive background activity during gameplay.
* Unexpected camera or microphone use.
* New unsigned executable launch.
* Significant startup slowdown.

Detection must be configurable and should avoid excessive notifications.

---

# 9.4 Deep Process Inspector

The Process Inspector should combine the strongest capabilities of Process Explorer and System Informer.

## 9.4.1 Process identity

Display:

* Friendly application name.
* Executable name.
* Process identifier.
* Parent process.
* Child processes.
* Start time.
* Running duration.
* User account.
* Session.
* File path.
* Command line.
* Working directory.
* Package identity.
* Publisher.
* Product name.
* File description.
* File version.
* Product version.
* Copyright metadata.
* Architecture.
* Digital signature status.
* Certificate chain.
* File hash.
* Installation source where detectable.
* Update source where detectable.

## 9.4.2 Process tree

The process tree should show:

* Parent-child relationships.
* Application grouping.
* Service grouping.
* Browser process roles.
* Process creation time.
* Process exit time.
* Orphaned processes.
* Restart relationships.
* Processes created by scheduled tasks.
* Processes created by services.
* Processes created by shell extensions.
* Processes launched by the user.

The tree should support both technical and simplified modes.

## 9.4.3 Handles and open resources

Display open:

* Files.
* Folders.
* Registry keys.
* Events.
* Mutexes.
* Sections.
* Pipes.
* Threads.
* Processes.
* Tokens.
* Jobs.
* Windows stations.
* Desktop objects.
* Synchronization objects.

Users should be able to search for which process is locking:

* A file.
* A folder.
* A device.
* A registry key.
* A named object.

For locked files, the product should offer:

* Close the owning application normally.
* Suspend the owning process.
* Release the handle where safe and supported.
* Schedule deletion after restart.
* Explain risks before forcing handle closure.

## 9.4.4 Modules and dynamic-link libraries

Display:

* Loaded modules.
* Module path.
* Publisher.
* Signature.
* Version.
* Load address.
* Memory size.
* Load time where available.
* Whether the module is shared.
* Whether the module is unsigned.
* Whether the module was loaded recently.
* Whether it is associated with an overlay, extension, hook, or security product.

## 9.4.5 Threads

Display:

* Thread identifier.
* Processor use.
* Start address.
* State.
* Wait reason.
* Priority.
* Processor affinity.
* User time.
* Kernel time.
* Context switches.
* Stack where supported.
* Associated module.
* Whether the thread appears responsible for a hang.

Thread termination must be hidden behind expert controls and strong warnings.

## 9.4.6 Tokens and privileges

Expert mode should display:

* Security identifier.
* Integrity level.
* Elevation state.
* User privileges.
* Group memberships.
* Application container state.
* Restricted token state.
* Capability identifiers.

## 9.4.7 Environment

Display:

* Environment variables.
* Current directory.
* Runtime information.
* Framework details where detectable.
* Java, .NET, Node.js, Python, Electron, Chromium, or WebView runtime association where detectable.
* Application package information.

## 9.4.8 Process performance history

Each process page should show:

* Current use.
* Historical use.
* Peaks.
* Average use.
* Total processor time.
* Disk reads and writes.
* Network transfer.
* Memory growth.
* Handle growth.
* Thread growth.
* Crash history.
* Hang history.
* Restart history.
* Energy impact.
* Temperature correlation.
* Rule history.

---

# 9.5 File Lock and Resource Search

The product should include a universal resource ownership search.

Users should be able to paste or select:

* A file.
* A folder.
* A port.
* An Internet Protocol address.
* A domain.
* A registry path.
* A module.
* A process identifier.
* A service.
* A window title.

The application should identify:

* Which process owns the resource.
* Why it may be using it.
* When access began.
* Whether the access is active or historical.
* Whether the resource is shared.
* Whether closing the process is safe.
* Whether another application depends on it.

This feature should be accessible from Windows File Explorer through an optional context-menu integration:

> Find what is using this file.

---

# 9.6 Performance Monitoring

## 9.6.1 Processor

Display:

* Total use.
* Per-core use.
* Performance-core and efficiency-core distinction where applicable.
* Clock frequency.
* Base frequency.
* Effective frequency.
* Processor temperature.
* Package power.
* Thermal throttling.
* Power-limit throttling.
* Interrupt activity.
* Deferred procedure call activity.
* Context switches.
* Virtualization state.
* Uptime.
* Process contribution.

## 9.6.2 Memory

Display:

* Physical memory.
* Available memory.
* In-use memory.
* Cached memory.
* Standby memory.
* Modified memory.
* Compressed memory.
* Commit charge.
* Commit limit.
* Page-file use.
* Hard faults.
* Memory pressure.
* Per-process private memory.
* Shared memory.
* Graphics memory interaction where relevant.

The application must explain that high memory utilization is not automatically a problem.

It should distinguish:

* Useful file cache.
* Active application memory.
* Memory compression.
* Page-file pressure.
* Actual memory shortage.

## 9.6.3 Storage

Display:

* Read throughput.
* Write throughput.
* Active time.
* Queue depth.
* Latency.
* Input/output operations per second.
* Per-process disk use.
* Per-file disk use where feasible.
* Disk health information.
* Available space.
* Temperature where available.
* Solid-state drive wear indicators where available.
* Trim state.
* Encryption state.
* File-system errors.
* Storage-related incidents.

## 9.6.4 Graphics processor

Display:

* Overall use.
* Per-engine use.
* Three-dimensional activity.
* Compute activity.
* Video encoding.
* Video decoding.
* Copy engine.
* Dedicated graphics memory.
* Shared graphics memory.
* Temperature.
* Power.
* Clock frequency.
* Thermal throttling.
* Per-process graphics processor use.
* Per-process graphics memory use.
* Active display adapter.
* Driver version.
* Graphics-related crashes.

## 9.6.5 Network

Display:

* Current upload.
* Current download.
* Per-application activity.
* Per-process activity.
* Active connections.
* Listening ports.
* Local address.
* Remote address.
* Domain resolution.
* Protocol.
* Connection state.
* Connection duration.
* Data transferred.
* Network adapter.
* Wireless signal where available.
* Latency where measured.
* Packet loss where measured.
* Network changes.
* Virtual private network state.
* Metered connection state.

## 9.6.6 Battery and power

Display:

* Charge percentage.
* Charging state.
* Estimated time remaining.
* Current discharge rate.
* Recent discharge history.
* Battery capacity.
* Design capacity.
* Full-charge capacity.
* Battery health.
* Cycle count where available.
* Per-application energy impact.
* Screen-related energy impact.
* Background activity.
* Sleep behavior.
* Connected standby activity.
* Wake sources.
* Power mode.
* Active performance rules.

## 9.6.7 Thermals and cooling

Display where hardware support exists:

* Processor temperature.
* Graphics processor temperature.
* Storage temperature.
* Motherboard sensors.
* Fan speeds.
* Thermal throttling.
* Thermal events.
* Temperature history.
* Applications active during thermal spikes.

The application must clearly state when sensor support is unavailable or incomplete.

---

# 9.7 Performance Rules and Optimization

This module should provide the persistent control capabilities expected from Process Lasso.

## 9.7.1 Rule triggers

Rules should activate based on:

* Application launch.
* Process launch.
* Application focus.
* Full-screen state.
* Game detection.
* External power connection.
* Battery state.
* Battery level.
* Thermal condition.
* Processor load.
* Graphics processor load.
* Memory pressure.
* User session.
* Time of day.
* Network connection.
* Active power mode.
* Device connection.

## 9.7.2 Rule actions

Rules should support:

* Set process priority.
* Set input/output priority.
* Set memory priority.
* Set processor affinity.
* Prefer performance cores.
* Prefer efficiency cores.
* Limit selected cores.
* Set power mode.
* Set application power preference.
* Enable or disable efficiency mode.
* Limit background processor contribution.
* Suspend selected background processes.
* Resume selected background processes.
* Prevent duplicate instances.
* Restart a process when it becomes unresponsive.
* Close a background application when a game starts.
* Restore the application after the game closes.
* Pause indexing.
* Pause selected updaters.
* Delay selected background tasks.
* Enable a focused performance session.
* Disable selected overlays.
* Change notification behavior through supported Windows interfaces.
* Trigger a user-defined script.
* Start or stop a service.
* Change network priority where supported.

## 9.7.3 Dynamic responsiveness protection

The application should detect processes that monopolize processor resources and temporarily reduce their impact without permanently changing the process configuration.

The system must:

* Avoid interfering with critical system processes.
* Avoid reducing foreground responsiveness.
* Keep a clear record of every automated intervention.
* Allow full disablement.
* Explain why an intervention occurred.
* Restore the original state automatically.
* Learn only from explicit user approval in early versions.

## 9.7.4 Application profiles

Users should be able to create profiles for:

* Games.
* Creative applications.
* Development environments.
* Video calls.
* Battery-saving sessions.
* Rendering.
* Large builds.
* Virtual machines.
* Streaming.
* General productivity.

Each profile should include:

* Trigger applications.
* Power mode.
* Processor rules.
* Graphics preference.
* Background application rules.
* Network rules.
* Notification behavior.
* Service behavior.
* Recording detail.
* Alerts.
* Restore behavior.

## 9.7.5 Rule simulation

Before applying a rule, the user should be able to preview:

* Which processes will be affected.
* What will change.
* How long the change lasts.
* Potential risks.
* How to undo it.

## 9.7.6 Rule conflict resolution

The product should detect conflicts such as:

* One rule sets high priority.
* Another sets below-normal priority.
* One rule assigns performance cores.
* Another restricts the process to efficiency cores.

The user should see rule precedence and the effective final policy.

---

# 9.8 Startup and Background Management

## 9.8.1 Startup inventory

The application should discover startup sources including:

* Windows startup applications.
* Registry startup entries.
* Startup folders.
* Scheduled tasks.
* Services.
* Application-specific background agents.
* Browser background modes.
* Packaged application startup tasks.
* Shell extensions.
* Drivers where relevant.
* Login scripts where relevant.

## 9.8.2 Startup explanation

Each startup entry should show:

* Friendly application name.
* Publisher.
* Purpose.
* Startup source.
* Estimated startup delay.
* Historical resource impact.
* Frequency of use.
* Last launch.
* Whether the application still functions when startup is disabled.
* What features may stop working.
* Whether the entry is necessary for security, drivers, synchronization, updates, notifications, or hardware support.

## 9.8.3 Startup control

Actions should include:

* Disable.
* Enable.
* Delay.
* Run only on external power.
* Run only after the system is idle.
* Run only after network connection.
* Run only when the related application opens.
* Remove orphaned startup entry.
* Restore previous state.

## 9.8.4 Boot analysis

The product should measure:

* Firmware duration where available.
* Windows boot duration.
* Login duration.
* Desktop-ready duration.
* Startup application duration.
* Service delays.
* Disk pressure.
* Processor pressure.
* Changes compared with previous boots.

The product should identify which changes caused boot regression.

---

# 9.9 Services and Scheduled Tasks

## 9.9.1 Service management

Display:

* Friendly service name.
* Internal service name.
* Description.
* Publisher.
* Executable path.
* Process host.
* Startup type.
* Current status.
* Dependencies.
* Dependent services.
* Trigger conditions.
* Service account.
* Last start.
* Last stop.
* Restart history.
* Failure history.
* Resource use.
* Associated application.

Actions:

* Start.
* Stop.
* Restart.
* Pause.
* Resume.
* Change startup behavior.
* View dependencies.
* View process.
* Create alert.
* Restore default setting where known.

Critical services should have strong protection.

## 9.9.2 Scheduled task management

Display:

* Task name.
* Folder.
* Publisher.
* Trigger.
* Last run.
* Next run.
* Last result.
* Executable.
* Arguments.
* User context.
* Privilege level.
* Resource history.
* Associated application.
* Whether the task runs while idle.
* Whether it wakes the computer.

Actions:

* Run.
* Stop.
* Disable.
* Enable.
* Edit through supported interfaces.
* View history.
* Create alert.
* Export definition.

---

# 9.10 Privacy Activity

The Privacy module should provide a clear activity history for sensitive capabilities.

## 9.10.1 Monitored capabilities

Where Windows interfaces allow:

* Camera.
* Microphone.
* Location.
* Screen capture.
* Clipboard access where observable and appropriate.
* Contacts.
* Calendar.
* Notifications.
* Bluetooth.
* Nearby devices.
* File-system access.
* Pictures library.
* Videos library.
* Documents library.

## 9.10.2 Privacy event details

Each event should show:

* Application.
* Process.
* Capability used.
* Start time.
* End time.
* Duration.
* Foreground or background state.
* User account.
* Application publisher.
* Signature status.
* Related window.
* Related network activity.
* Whether the user was active.
* Whether the device indicator was visible.

## 9.10.3 Privacy alerts

Users should be able to configure:

* Alert whenever a selected capability is used.
* Alert only for background use.
* Alert only for unknown applications.
* Alert only for unsigned applications.
* Alert outside selected hours.
* Alert when the computer is locked.
* Alert when a capability remains active longer than a threshold.

The application should never imply malicious behavior without evidence.

---

# 9.11 Application Inventory

The Applications module should unify installed software, active applications, and historical behavior.

Display:

* Application name.
* Publisher.
* Version.
* Installation date.
* Last update.
* Last launch.
* Total active time.
* Background activity.
* Startup status.
* Installed services.
* Installed scheduled tasks.
* Installed drivers.
* Network activity.
* Privacy activity.
* Crash history.
* Hang history.
* Resource history.
* Digital signature.
* Installation source.
* Update source.
* Associated processes.
* Associated startup entries.
* Associated file extensions.
* Storage use.

The application should identify abandoned or orphaned components, but it must not recommend removal solely because an application has not been used recently.

---

# 9.12 Network Inspector

The Network module should combine easy application-level understanding with detailed technical inspection.

## 9.12.1 Connection table

Display:

* Application.
* Process.
* Local address.
* Local port.
* Remote address.
* Remote port.
* Resolved domain.
* Protocol.
* Connection state.
* Start time.
* Duration.
* Upload.
* Download.
* Transfer rate.
* Encryption indication where detectable.
* Network adapter.
* Geographic information only when based on an explicitly enabled local or remote database.
* Publisher.
* Signature status.

## 9.12.2 Listening ports

Display:

* Port.
* Protocol.
* Owning process.
* Service.
* Bind address.
* Exposure scope.
* Firewall status where available.
* First observed time.
* Last observed time.

## 9.12.3 Historical network activity

Users should be able to answer:

* Which application uploaded data while the computer was idle?
* Which application contacted a new domain?
* Which application transferred the most data?
* Which background process repeatedly reconnects?
* What network activity occurred during a performance incident?

## 9.12.4 Network actions

Where safe and supported:

* End connection.
* Block application through Windows Firewall.
* Create temporary block.
* Create alert.
* Copy endpoint.
* Resolve domain.
* Open process.
* View historical activity.
* Reverse firewall action.

---

# 9.13 System Changes

The product should record relevant system changes.

Examples:

* Application installation.
* Application update.
* Application removal.
* Driver installation.
* Driver update.
* Windows update.
* Service installation.
* Service configuration change.
* Scheduled task creation.
* Startup entry addition.
* Startup entry removal.
* Firewall rule change.
* Power-plan change.
* Hardware connection.
* Default application change.
* Relevant system setting change.
* Product-applied change.

Each change should show:

* Time.
* Responsible installer or process where known.
* User account.
* Before state.
* After state.
* Related files.
* Related services.
* Related processes.
* Possible impact.
* Reversal options where available.

This module is essential for answering:

> What changed before the problem started?

---

# 9.14 Reliability and Crash Analysis

The product should unify crash and reliability information.

Display:

* Application crashes.
* Application hangs.
* Blue-screen events.
* Unexpected restarts.
* Service failures.
* Driver failures.
* Windows update failures.
* Installation failures.
* Hardware error reports where available.
* Repeated restart loops.
* Faulting module.
* Exception code.
* Crash time.
* Resource state before the crash.
* Recent related system changes.

The product should correlate:

* Crash event.
* Process history.
* Memory pressure.
* Thermal state.
* Driver update.
* Application update.
* Module load.
* Network state.

---

# 9.15 Diagnostics Engine

The diagnostics engine should transform system data into structured investigations.

## 9.15.1 Diagnostic question types

The engine should answer:

* Why was the computer slow?
* Why did the game stutter?
* Why did the application crash?
* Why is memory use high?
* Why is disk use high?
* Why is the battery draining?
* Why is the computer hot?
* Why are the fans loud?
* Why did startup become slower?
* Why can this file not be deleted?
* Why does this application keep reopening?
* What used the camera or microphone?
* What changed recently?
* Is a background application interfering with my work?
* Which application consumes the most resources over time?
* Which application caused repeated processor spikes?
* Why does the computer wake from sleep?
* Why is the network slow?
* Which process owns this port?
* Why does a service keep restarting?

## 9.15.2 Diagnostic output structure

Every diagnostic result should include:

### Observed issue

A plain-language statement of what happened.

### Time period

Exact incident period.

### Evidence

Measured facts such as:

* Resource peaks.
* Process activity.
* Service changes.
* Network activity.
* Thermal events.
* Crashes.
* Updates.

### Likely contributing factors

Ranked by confidence.

### Confidence level

* Confirmed.
* High confidence.
* Medium confidence.
* Low confidence.
* Insufficient evidence.

### Alternative explanations

Relevant competing possibilities.

### Recommended action

A safe action with expected effect.

### Risk

Possible side effects.

### Reversibility

How the action can be undone.

### Verification plan

How the application will determine whether the problem improved.

## 9.15.3 Before-and-after validation

When the user applies a recommendation, the product should create an experiment record.

Example:

* Baseline game session.
* Background launcher disabled.
* New game session.
* Compare frame-time-related resource spikes.
* Report whether the change improved the selected metric.

The product should avoid declaring success based on insufficient data.

---

# 9.16 Natural-Language Analysis and MCP Integration

> **Direction (2026-07-13):** Atlas does **not** host an artificial-intelligence model or generate conversational answers. It collects, analyzes, and exposes system *evidence*. Natural-language reasoning is provided two ways: (1) an in-app deterministic assistant that answers a fixed repertoire of questions from templates and playbooks, with no model; and (2) an optional, read-only **Model Context Protocol (MCP) server** that exposes grounded query tools to the user's own MCP-compatible client (such as Claude or ChatGPT), which supplies the model and the conversation. Atlas is the trusted evidence provider; the user chooses the AI client.

Users should be able to ask questions such as:

* What caused the processor spike at 7:30 PM?
* Which applications used the most memory yesterday?
* Did anything use my microphone while the computer was locked?
* Why did Rocket League stutter?
* What changed after the latest update?
* Which startup applications can I safely delay?
* Find the process preventing me from deleting this folder.
* Show applications that contacted new domains this week.
* Compare battery drain today with yesterday.
* Is Brave using more memory than usual?
* Which process repeatedly crashes?
* What would happen if I stop this service?

The in-app deterministic assistant answers the supported question types directly from recorded evidence and the diagnostics playbooks. Broader or open-ended conversation is handled by the user's MCP client through the MCP tools below.

## 9.16.1 Grounded, citation-ready evidence

Every result Atlas returns — from the in-app assistant and from the MCP tools — must be structured and carry the evidence needed to cite it:

* Timeline range.
* Process identity.
* Event.
* Metric.
* Change.
* Rule.
* Configuration item.
* Confidence level.
* Missing-data markers.
* Retention or sensor limitations.

The in-app assistant displays only claims backed by this evidence. For the MCP path, Atlas guarantees that its tools return **citation-ready** evidence with these fields. Atlas **cannot** guarantee that the external model's final answer contains no unsupported claims: Atlas controls the tool results, but the MCP client controls the conversation and the final response. The product must present this honestly — it provides citation-ready evidence, not a guarantee that every external answer is cited.

## 9.16.2 MCP server (read-only)

The optional MCP server should expose controlled, read-only tools such as:

* `query_timeline`
* `top_consumers`
* `find_events`
* `diff_periods`
* `explain_process`
* `get_incident`
* `get_playbook_result`
* `list_system_changes`
* `find_crashes`

Requirements:

* The MCP server exposes **read-only** tools only. It must never be able to terminate, suspend, or reconfigure anything; any action a model suggests becomes a recommendation the user confirms through the normal in-app consent and broker flow.
* It hosts no model and performs no inference. The user's MCP client provides the model.
* Each tool result is self-describing (the §9.16.1 fields).

## 9.16.3 Explicit data boundaries

When an MCP client calls a tool, the returned system data may be sent to that client's model provider. The product must treat the MCP surface as an external boundary:

* MCP is disabled by default and requires explicit user enablement.
* Read-only tools only.
* Sensitive fields excluded by default; redaction defaults on for MCP and is stricter than for local in-app views.
* Configurable redaction for file paths, user names, domains, window titles, command lines, and application names.
* Result-size and time-range limits per tool.
* A clear warning that returned information leaves Atlas's security boundary.
* Tool-call auditing that records exactly what each tool returned (or a hash plus field summary).
* The ability to revoke MCP access immediately.

Atlas may not be able to preview the client's complete final prompt, but it can show and log exactly what each MCP tool returned.

## 9.16.4 Stated limitations

Both the in-app assistant and the evidence returned to MCP clients must state when:

* Data is missing.
* Monitoring was disabled.
* The event occurred outside retention.
* Sensor support is unavailable.
* Correlation does not prove causation.
* A conclusion depends on incomplete evidence.

---

# 9.17 Alerts and Notifications

The alert system should support:

* Processor threshold.
* Memory pressure.
* Disk latency.
* Graphics memory exhaustion.
* Thermal threshold.
* Battery discharge threshold.
* Application crash.
* Application hang.
* Service failure.
* Repeated process restart.
* New startup entry.
* New service.
* New scheduled task.
* New unsigned executable.
* Camera use.
* Microphone use.
* Location use.
* Unexpected network upload.
* New listening port.
* Rule activation.
* Rule failure.
* Significant startup regression.
* Device wake event.
* Storage health warning.

Alert configuration should support:

* Threshold.
* Duration.
* Application scope.
* User scope.
* Time range.
* Power state.
* Foreground or background condition.
* Notification method.
* Cooldown.
* Grouping.
* Severity.

The application must prevent notification fatigue through:

* Sensible defaults.
* Event grouping.
* Repeated-event suppression.
* Weekly alert-quality review.
* Easy tuning.

---

# 9.18 Reports and Export

Users should be able to generate reports for:

* Performance incident.
* Gameplay session.
* Battery session.
* Startup.
* Application behavior.
* Process behavior.
* Service failures.
* Privacy activity.
* Network activity.
* System changes.
* Crash investigation.
* Before-and-after comparison.
* Full device health.

Export formats:

* Portable Document Format.
* Hypertext Markup Language.
* Comma-separated values.
* JavaScript Object Notation.
* Plain text.
* Diagnostic bundle.

Reports should support privacy redaction.

Redaction options:

* User names.
* Computer name.
* File paths.
* Internet Protocol addresses.
* Domains.
* Window titles.
* Command-line arguments.
* Application names.
* Serial numbers.
* Device identifiers.

A report should include enough evidence for support without exposing unrelated personal activity.

---

# 9.19 Session and User Management

For multi-user systems, display:

* Signed-in users.
* Active sessions.
* Disconnected sessions.
* Per-session resource use.
* Applications per user.
* Processes per user.
* Network activity per user.
* Session start time.
* Session state.

Administrative actions may include:

* Disconnect session.
* Sign out session.
* Send message where supported.
* Inspect session processes.

Strong confirmation is required for actions affecting another user.

---

# 9.20 Window and Interface Inspection

The application should provide a view of:

* Open windows.
* Hidden windows.
* Window titles.
* Owning processes.
* Window state.
* Desktop.
* Display.
* Responsiveness.
* Parent window.
* Child windows.
* Off-screen windows.

Actions:

* Bring to front.
* Move to current display.
* Restore.
* Minimize.
* Maximize.
* Close normally.
* Identify process.
* Capture diagnostic metadata.

This is useful when applications are running but inaccessible.

---

# 9.21 Efficiency Mode

The product should improve on Windows Efficiency Mode.

It should explain:

* What Efficiency Mode does.
* Which processes are affected.
* Why the mode is active.
* Whether the application enabled it.
* Whether Windows enabled it.
* Whether a product rule enabled it.
* Whether it may affect responsiveness.
* Whether it is temporary.
* Whether it will return after restart.

Users should be able to:

* Enable temporarily.
* Disable temporarily.
* Create persistent preference.
* Apply to child processes.
* Exclude selected processes.
* Compare performance before and after.

---

# 9.22 Safe End Task Experience

Ending a process should be redesigned.

The user should see:

* Application name.
* Process role.
* Open windows.
* Unsaved-work risk.
* Child processes.
* Dependent services.
* Whether the process is critical.
* Whether it will restart.
* Whether the application can be closed normally.
* Whether suspend is safer.
* Whether restarting is preferable.

Action hierarchy:

1. Close normally.
2. Wait and inspect.
3. Restart application.
4. Suspend.
5. End selected process.
6. End process tree.
7. Force termination.

Critical system processes should require expert mode and explicit risk acknowledgment.

---

# 10. Information Architecture Detail

## 10.1 Overview

Purpose: Immediate understanding.

Sections:

* Current status.
* Active pressure.
* Recent incidents.
* Recommended attention.
* Privacy activity.
* Recent changes.
* Active rules.

## 10.2 Live Activity

Purpose: Real-time monitoring and actions.

Views:

* Applications.
* Processes.
* Resource groups.
* Windows.
* Users.

## 10.3 Timeline

Purpose: Historical investigation.

Views:

* System timeline.
* Application timeline.
* Incident timeline.
* Comparison.

## 10.4 Applications

Purpose: Application-centric understanding.

Views:

* Installed.
* Running.
* Background.
* High impact.
* Recently changed.
* Crashing.
* Network-active.
* Privacy-active.

## 10.5 Processes

Purpose: Technical process analysis.

Views:

* Tree.
* Flat list.
* Services.
* Jobs.
* Suspended.
* Unsigned.
* Recently started.
* Recently exited.

## 10.6 Performance

Purpose: Hardware and resource analysis.

Views:

* Processor.
* Memory.
* Storage.
* Graphics processor.
* Network.
* Battery.
* Thermals.

## 10.7 Rules and Optimization

Purpose: Persistent behavior control.

Views:

* Profiles.
* Rules.
* Active interventions.
* History.
* Conflicts.
* Recommendations.

## 10.8 Startup and Background

Purpose: Startup and idle activity control.

Views:

* Startup applications.
* Services.
* Scheduled tasks.
* Browser background activity.
* Boot history.

## 10.9 Privacy

Purpose: Sensitive capability visibility.

Views:

* Camera.
* Microphone.
* Location.
* Screen capture.
* Other permissions.
* Alerts.

## 10.10 Diagnostics

Purpose: Explain problems.

Views:

* Ask a question.
* Active incident.
* Past investigations.
* Experiments.
* Recommendations.
* Results.

---

# 11. Design Requirements

## 11.1 Visual direction

The interface should feel native to Windows 11 without directly copying Task Manager.

Characteristics:

* Clear hierarchy.
* High information density without visual clutter.
* Calm neutral surfaces.
* Strong typography.
* Limited decorative effects.
* Meaningful color use.
* Consistent icons.
* Smooth but restrained animation.
* Accessible contrast.
* Keyboard-first navigation.
* Touch support where practical.
* High-resolution display support.

## 11.2 Color semantics

Colors should represent stable meanings:

* Normal.
* Informational.
* Attention.
* Warning.
* Critical.
* Historical selection.
* Automated intervention.
* Privacy activity.
* System process.
* User application.
* Service.
* Suspended process.

Color should never be the only information carrier.

## 11.3 Charts

Charts should:

* Remain readable under high data density.
* Support zoom.
* Support hover inspection.
* Support synchronized cursors.
* Show peaks without hiding short events.
* Indicate missing data.
* Distinguish measured data from estimated data.
* Allow application overlays.
* Allow baseline comparison.
* Avoid unnecessary animation during live monitoring.

## 11.4 Responsive layout

The product should support:

* Compact window.
* Standard desktop window.
* Wide monitor.
* Multi-monitor.
* High scaling.
* Full-screen diagnostics.
* Collapsible side navigation.
* Resizable columns.
* Saved layouts.

## 11.5 Accessibility

Requirements:

* Full keyboard navigation.
* Screen-reader labels.
* High-contrast mode.
* Reduced-motion mode.
* Text scaling.
* Color-independent status indication.
* Clear focus states.
* Accessible charts through summary tables.
* Customizable time and number formatting.

---

# 12. Performance Requirements

These targets should guide implementation.

## 12.1 Startup

* Main interface should appear within 500 milliseconds on a healthy modern system after background service initialization.
* Cold launch should remain under 1.5 seconds on supported baseline hardware.
* Critical process list should become visible before nonessential historical charts finish loading.

## 12.2 Background processor usage

Idle background processor use target:

* Less than 0.2% average on a typical modern laptop.
* Short sampling spikes are acceptable if the long-term average remains low.
* User-configurable high-detail recording may use more resources, but impact must be displayed.

## 12.3 Memory use

Targets:

* Background service under 100 megabytes in standard mode.
* User interface under 200 megabytes in standard use.
* Historical data should be streamed and paged rather than fully loaded into memory.
* Expert inspection views may temporarily use more memory.

## 12.4 Disk impact

* Use buffered and batched writes.
* Avoid one disk write per metric sample.
* Use an append-efficient local data design.
* Compact historical data during idle periods.
* Allow users to limit storage size.
* Show current database size.
* Avoid unnecessary solid-state drive write amplification.

## 12.5 Graphics processor use

* The interface should not continuously consume significant graphics processor resources while minimized.
* Charts should reduce refresh frequency when not visible.
* Animation should pause when the application is not active.
* The product should include a low-rendering mode.

## 12.6 Battery impact

* Sampling frequency should adapt on battery.
* High-detail monitoring should require explicit selection or incident activation.
* The product should show its own energy impact.
* Background monitoring should respect connected standby limitations.

## 12.7 Responsiveness under pressure

The application should remain usable when:

* Processor use is 100%.
* Memory pressure is high.
* Disk utilization is saturated.
* A foreground application is not responding.

This may require:

* Dedicated high-priority control thread.
* Minimal dependency on the monitored user-interface process.
* Separate collection service.
* Lightweight emergency interface.

---

# 13. Technical Architecture

## 13.1 Main components

### Collection service

Responsible for:

* Process events.
* Resource metrics.
* Service events.
* Scheduled task events.
* Application events.
* Privacy events.
* Network events.
* Hardware sensors.
* System changes.
* Reliability events.

### Local data engine

Responsible for:

* Time-series storage.
* Event storage.
* Indexing.
* Retention.
* Compression.
* Querying.
* Redaction.
* Export.

### Rules engine

Responsible for:

* Trigger evaluation.
* Action execution.
* Conflict resolution.
* Rollback.
* Audit logging.

### Diagnostic engine

Responsible for:

* Pattern detection.
* Event correlation.
* Baseline comparison.
* Confidence scoring.
* Recommendation generation.
* Outcome verification.

### User interface

Responsible for:

* Live views.
* Historical views.
* Search.
* Configuration.
* Reports.
* Diagnostics.
* Accessibility.

### Privileged broker

Responsible for privileged operations such as:

* Service control.
* Process termination.
* Affinity changes.
* Priority changes.
* Firewall changes.
* Protected information access where allowed.

The privileged component should expose the minimum required surface.

## 13.2 Process isolation

The user interface, collection service, privileged broker, MCP server, and update mechanism should be isolated.

A user-interface crash must not stop historical collection.

An MCP-server failure must not affect monitoring. The MCP server is an optional, read-only process and is absent entirely unless the user enables it.

## 13.3 Data model

Core entities:

* Device.
* User session.
* Application.
* Process.
* Thread.
* Service.
* Scheduled task.
* Startup entry.
* File.
* Module.
* Network endpoint.
* Resource sample.
* Event.
* Incident.
* Rule.
* Rule execution.
* Recommendation.
* Action.
* Experiment.
* Report.
* Privacy capability.
* System change.
* Hardware sensor.

## 13.4 Sampling strategy

Use adaptive sampling:

* Low-frequency sampling during idle periods.
* Higher frequency when resource changes accelerate.
* Event-triggered high-detail recording.
* User-initiated incident recording.
* Per-process prioritization.
* Lower frequency for stable background processes.
* Immediate capture for process creation and exit.

## 13.5 Data retention strategy

Use multiple layers:

* Recent high-resolution samples.
* Medium-resolution daily data.
* Event-preserved long-term summaries.
* Permanent user bookmarks.
* User-selected incident archives.

## 13.6 Driver requirement

A kernel driver should be used only for capabilities that cannot be implemented reliably through documented Windows interfaces.

Before including a driver, the team must assess:

* Security risk.
* Signing requirements.
* Compatibility.
* Update complexity.
* Performance cost.
* Crash risk.
* Whether a user-mode alternative is sufficient.

The product should function in a reduced-capability mode without the driver where possible.

---

# 14. Security Requirements

## 14.1 Least privilege

The main application should run without administrative privileges.

Elevation should occur only when necessary.

The product should explain why elevation is required.

## 14.2 Code signing

All executables, services, drivers, and updates must be digitally signed.

## 14.3 Secure updates

Updates should include:

* Signed packages.
* Signature verification.
* Rollback.
* Staged deployment.
* Release notes.
* Security channel for urgent fixes.

## 14.4 Data protection

Local history should support:

* Access control.
* Optional encryption.
* Per-user separation.
* Secure deletion where feasible.
* Clear retention controls.
* Export confirmation.
* Privacy redaction.

## 14.5 Audit log

Record:

* User actions.
* Automated rule actions.
* Privileged operations.
* Settings changes.
* Data exports.
* MCP enablement and each MCP tool call, including exactly what the tool returned (or a hash plus field summary).
* Rollbacks.
* Update operations.

## 14.6 External integrations

No third-party integration should receive system data without explicit user configuration.

---

# 15. Privacy Requirements

The product should not require an account for local use.

The product should not collect by default:

* Application usage history.
* File paths.
* Window titles.
* Domains.
* Process command lines.
* Microphone or camera history.
* User names.
* Diagnostic reports.

Optional telemetry should be:

* Off by default or clearly requested during onboarding.
* Aggregated.
* Minimized.
* Inspectable.
* Revocable.
* Deletable.

Users should be able to view exactly what telemetry would be sent.

---

# 16. Onboarding

The onboarding process should ask users to choose a mode.

## 16.1 Standard mode

Includes:

* Real-time monitoring.
* Seventy-two-hour history.
* Basic diagnostics.
* Startup management.
* Privacy alerts.
* Safe actions.

## 16.2 Low-impact mode

Includes:

* Reduced sampling.
* Minimal background collection.
* Lower chart refresh.
* Shorter history.
* No continuous deep inspection.

## 16.3 Advanced mode

Includes:

* Longer retention.
* More process detail.
* Network history.
* Deep process events.
* Advanced alerts.
* Rules.
* Expert controls.

The user should be able to change modes later.

Onboarding should clearly explain:

* What is monitored.
* Where data is stored.
* How much storage may be used.
* Which capabilities require elevation.
* Whether a driver is installed.
* Whether the read-only MCP server is enabled, and that when it is, data returned to an MCP client leaves Atlas's security boundary for that client's model provider.

---

# 17. Example User Flows

## 17.1 Frozen application

1. User opens Atlas.
2. The application identifies an unresponsive application.
3. It shows how long the application has been unresponsive.
4. It shows processor, disk, and wait-state information.
5. It indicates whether the application may still be processing.
6. It offers Wait, Close Normally, Restart, Suspend, or Force End.
7. It warns about unsaved work.
8. It records the outcome.

## 17.2 Game stutter investigation

1. User presses the incident-marker shortcut during a stutter.
2. The application saves a high-resolution time window around the event.
3. The user opens the incident.
4. The system shows processor, graphics processor, memory, disk, network, temperature, and background activity.
5. It ranks likely contributors.
6. It identifies a launcher update, memory pressure, or thermal throttling.
7. It proposes a reversible game profile.
8. The user applies it.
9. The application compares the next session.
10. It reports whether the stutter pattern improved.

## 17.3 Locked file

1. User right-clicks a file in File Explorer.
2. User selects Find what is using this file.
3. Atlas identifies the owning process and open handle.
4. It explains the process.
5. It offers to close the application normally.
6. If needed, it offers a force-release option with risk explanation.
7. It confirms whether the file is now available.

## 17.4 High memory use

1. User sees 84% memory utilization.
2. Atlas separates active application memory, cache, compression, and page-file pressure.
3. It explains whether the system is actually under memory stress.
4. It shows the largest applications and recent growth.
5. It identifies a browser tab, extension, or process group where possible.
6. It recommends closing selected high-impact items rather than randomly ending processes.

## 17.5 Microphone use while locked

1. The product records a microphone event.
2. The event shows the application, process, duration, signature, foreground state, and network activity.
3. The user receives an alert because the computer was locked.
4. The application avoids accusing the application of malicious behavior.
5. The user can inspect permissions and create a future alert.

## 17.6 Startup slowdown

1. The application compares the last ten boots.
2. It detects a significant increase in desktop-ready time.
3. It identifies a newly installed startup application and delayed service.
4. It explains what disabling or delaying each item would affect.
5. The user applies a delayed-start rule.
6. The next boot is measured.
7. The application reports the actual improvement.

---

# 18. Feature Prioritization

## 18.1 Minimum viable product

The initial release should include:

* Real-time application and process list.
* Application grouping.
* Processor, memory, disk, network, and graphics processor monitoring.
* Process details.
* Process tree.
* Safe process actions.
* Search.
* Seventy-two-hour historical timeline.
* Application launch and exit history.
* Resource spikes.
* Basic privacy events.
* Basic startup management.
* Basic service management.
* Incident bookmarks.
* Basic diagnostic summaries.
* Local storage.
* Exportable incident report.
* Low-overhead collection.
* Modern Windows interface.

## 18.2 Second release

Add:

* Deep handles.
* Module inspection.
* File-lock search.
* Threads.
* Persistent performance rules.
* Application profiles.
* Boot analysis.
* Scheduled task management.
* Detailed network inspector.
* Battery and thermal analysis.
* Before-and-after experiments.
* Read-only MCP server exposing grounded query tools to user-configured MCP clients.
* Advanced privacy alerts.

## 18.3 Third release

Add:

* Advanced rule automation.
* Dynamic responsiveness protection.
* Extended historical retention.
* Crash correlation.
* Driver and system-change tracking.
* Expert security metadata.
* Remote support bundle.
* Command-line interface.
* Scriptable automation.
* Plugin framework.

---

# 19. Success Metrics

## 19.1 Product utility

* Median time to identify a high-resource process.
* Median time to diagnose a historical performance incident.
* Percentage of incidents with a clear evidence-backed explanation.
* Percentage of recommendations successfully verified.
* Percentage of actions reversed.
* Percentage of users who find the cause without opening another system tool.

## 19.2 Performance

* Idle processor consumption.
* Idle memory consumption.
* Daily disk writes.
* Battery impact.
* Cold launch time.
* Search response time.
* Timeline query response time.
* Emergency action response time under system load.

## 19.3 Reliability

* Collection-service uptime.
* Data-loss rate.
* Application crash rate.
* Driver-related failure rate.
* Rule execution failure rate.
* Rollback success rate.

## 19.4 User trust

* Percentage of recommendations with viewed evidence.
* Percentage of MCP tool calls whose returned evidence carried complete grounding fields (confidence, evidence IDs, missing-data markers).
* Privacy-alert false-positive rate.
* Destructive-action cancellation rate.
* User-reported confidence in explanations.

---

# 20. Acceptance Criteria

The product will be considered ready for public release when:

1. It can display and update active applications and processes with lower or comparable overhead to Windows Task Manager.
2. It can preserve and query at least seventy-two hours of historical activity.
3. It can identify the main processes active during a selected historical processor, memory, disk, network, or graphics processor spike.
4. It can distinguish application groups from individual helper processes.
5. It can safely terminate, suspend, resume, and restart supported processes.
6. It can warn users before terminating critical or data-bearing applications.
7. It can inspect process relationships, signatures, paths, and command lines.
8. It can identify common file locks.
9. It can manage common startup entries.
10. It can display common service details and dependencies.
11. It can explain high memory use beyond a single utilization percentage.
12. It can show privacy-sensitive activity where Windows provides reliable interfaces.
13. It can generate a redacted diagnostic report.
14. It can operate without an account.
15. It can operate without sending system data to external services.
16. It can show its own resource consumption.
17. It can recover from user-interface failure without losing background monitoring.
18. It can roll back product-applied configuration changes.
19. It can state uncertainty when diagnostic evidence is incomplete.
20. It remains responsive during high system load.

---

# 21. Major Risks

## 21.1 Scope expansion

Combining several mature tools creates a large product surface.

Mitigation:

* Build one coherent architecture.
* Prioritize shared data collection.
* Release capabilities progressively.
* Avoid copying every obscure feature before validating core workflows.

## 21.2 Monitoring overhead

Continuous high-resolution recording may create processor, graphics processor, disk, and battery costs.

Mitigation:

* Adaptive sampling.
* Event-based collection.
* Buffered writes.
* Reduced rendering.
* User-selectable modes.
* Transparent self-monitoring.

## 21.3 Windows compatibility

Internal Windows behavior changes between builds.

Mitigation:

* Prefer documented interfaces.
* Isolate version-specific collectors.
* Maintain compatibility testing across supported builds.
* Provide reduced functionality when a capability is unavailable.

## 21.4 Privileged access

Deep process inspection and control can create security risks.

Mitigation:

* Least privilege.
* Small privileged broker.
* Signed components.
* Strict inter-process communication.
* Security review.
* No unnecessary kernel driver.

## 21.5 False diagnosis

The product may confuse correlation with causation.

Mitigation:

* Confidence levels.
* Alternative explanations.
* Evidence links.
* Controlled experiments.
* Avoid absolute claims without proof.

## 21.6 User damage

Users may stop important processes or disable required services.

Mitigation:

* Safe defaults.
* Explanations.
* Critical-process protection.
* Reversible actions.
* Restore points.
* Expert-mode barriers.

## 21.7 Privacy sensitivity

Historical system activity can reveal personal behavior.

Mitigation:

* Local-first storage.
* Short default retention.
* Exclusions.
* Redaction.
* Encryption.
* No account requirement.
* Explicit external-sharing consent.

---

# 22. Competitive Positioning

## Against Windows Task Manager

Atlas should provide:

* Better application grouping.
* Historical data.
* Clear explanations.
* Safer process actions.
* Stronger search.
* Performance policies.
* Privacy visibility.
* Diagnostic recommendations.
* Better process relationships.
* Better startup context.
* Before-and-after verification.

## Against Process Explorer

Atlas should provide:

* Equivalent common investigation capabilities.
* Better design.
* Plain-language explanations.
* Historical context.
* Application-level grouping.
* Broader hardware monitoring.
* Rules and optimization.
* Less intimidating default interface.

## Against System Informer

Atlas should provide:

* Comparable deep inspection for common workflows.
* Better progressive disclosure.
* Stronger historical analysis.
* Better diagnosis.
* Safer actions.
* Lower interface complexity.
* Stronger application-centric views.

## Against Process Lasso

Atlas should provide:

* Persistent priority and affinity rules.
* Dynamic responsiveness management.
* Power-mode profiles.
* Application-specific optimization.
* Rule conflict visibility.
* Historical evidence showing whether a rule helped.
* A broader monitoring and diagnosis platform.

## Against Task Manager DeLuxe

Atlas should provide:

* Similar breadth.
* Better visual hierarchy.
* Stronger usability.
* Historical monitoring.
* Diagnostic workflows.
* Better integration between modules.
* Modern design.
* Lower cognitive load.

## Against AppControl

Atlas should provide:

* Historical timeline.
* Privacy activity.
* Application and service events.
* Stronger process depth.
* More flexible retention.
* Persistent performance controls.
* File-lock investigation.
* Startup and scheduled-task management.
* Deeper network inspection.
* Before-and-after experiments.
* Clearer evidence and confidence.
* Lower and more transparent resource consumption.

---

# 23. Product Differentiator

The product’s main differentiator should not be the number of columns, charts, or technical controls.

The differentiator is the complete loop:

> Observe → Record → Detect → Explain → Recommend → Act → Verify → Reverse

Existing tools usually cover only one or two stages.

Windows Task Manager observes and acts.

Process Explorer inspects.

Process Lasso acts through persistent rules.

AppControl records and partially explains.

Atlas should connect the entire workflow in one coherent product.

---

# 24. Final Product Definition

Atlas is a Windows system intelligence and control application designed to replace fragmented process, performance, privacy, startup, service, and diagnostic utilities.

It should provide:

* The immediacy of Windows Task Manager.
* The process depth of Process Explorer.
* The technical visibility of System Informer.
* The persistent optimization rules of Process Lasso.
* The breadth of Task Manager DeLuxe.
* The historical timeline of AppControl.
* The reliability context of Reliability Monitor.
* The event context of Event Viewer.
* The hardware visibility of performance monitoring tools.
* A clearer and safer interface than all of them.

The product should not force the user to interpret dozens of disconnected measurements. It should expose raw data when needed, but its primary responsibility is to transform system activity into understandable, evidence-backed, reversible decisions.
