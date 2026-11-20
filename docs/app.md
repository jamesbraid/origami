# The Origami app

[Back to the README](../README.md)

Origami has a desktop app for Linux, macOS (Apple Silicon) and Windows. It
keeps a library of machines, starts them, shows their screens and serial
consoles, and edits their settings. It runs the same machines as the
[`origami` command](usage.md) and uses the same machine directories, so you can
create a machine in one and run it from the other.

| Linux | macOS | Windows |
|---|---|---|
| ![Linux](images/app/linux-running.png) | ![macOS](images/app/macos-library.png) | ![Windows](images/app/windows-running.png) |

The walkthrough below shows the Linux app; macOS and Windows have the same
windows with their own look and shortcuts.

This guide follows one session: create a machine, pick an engine, start it,
use the console, pause it, take a screenshot, and look at Diagnostics, the CPU
panel and About.

## Get the app

Pick the app archive for your computer from the
[release page](https://github.com/jamesbraid/origami/releases):
`origami-<version>-linux-x86_64.tar.gz`, `origami-<version>-linux-arm64.tar.gz`,
`origami-<version>-macos-universal.zip` (one download for Apple silicon and
Intel Macs), `origami-<version>-windows-x86_64.zip` or
`origami-<version>-windows-arm64.zip`. The app archive includes
the `origami` command, so you do not need the `origami-cli-` archive beside
it. Extract it, keep the extracted files together, and start `bin/origami-gtk`
(Linux), `Origami.app` (macOS) or the executable in the `app` folder (Windows).
The first time a machine needs
firmware, the app downloads and checks it, as `origami create` does.

A macOS build that is not signed, such as one from a fork or a development
build, is blocked by Gatekeeper. Run `xattr -dr com.apple.quarantine Origami.app`
once after extracting. Signed releases need no such step.

The library is a folder of machine directories. The app starts with:

| Platform | Library folder |
|---|---|
| Linux | `~/.local/share/origami/machines` |
| macOS | `~/Library/Application Support/Origami/Machines` |
| Windows | `%LocalAppData%\Origami\Machines` |

Machines you made with `origami create` appear once their directory is in that
folder. On Linux, `origami-gtk --library DIR` opens another folder; on macOS
and Windows use `--machines DIR` and `--library DIR` respectively.

## The window

![The Origami window with one stopped machine](images/app/linux-library.png)

The library is on the left, one row per machine with its state. Selecting a
row shows the machine on the right: its picture, name, state and a Start
button, with Memory, Nodes and Topology below. Double-click a row, or use
Open in New Tab in its right-click menu, to keep several machines open in
tabs. The toolbar has Start or Power Off, Pause, the pointer mode, the
Console, Settings and the main menu. The CPU button shows the
[CPU panel](#the-cpu-panel). A machine with no graphics opens straight to its
console.

## Machines from an older Origami

A machine made by an older release shows a **Needs upgrade** pill in the
library and cannot start. Open it and choose Upgrade (on Linux, opening it
shows an Upgrade dialog). This is [`origami upgrade`](usage.md#upgrading-a-machine):
flash, NVRAM, drives, network settings and the Ethernet address are kept, and
what the guest will notice at its next boot is listed when it is done. A
machine that needs firmware to be rebuilt downloads it. If that fails, the
message gives the `origami upgrade` command that takes local firmware files.
If an upgrade fails, the reason is shown. The machine file stays as it was unless the upgrade had already rewritten it.

## Create a machine

Choose New Machine (the **+** above the library, or Ctrl+N; Cmd+N on macOS).
The dialog asks for three things.

![The Engine list open in the New Machine dialog, offering qemu and iris](images/app/linux-engine.png)

**Engine** is the emulator that runs the machine. QEMU runs every machine
Origami knows. iris is a second, experimental engine for the Indy, Indigo2
and Octane2. When a model is offered by one engine only, the list has one entry.

![New Machine with iris chosen, the Indy model and a name typed](images/app/linux-new-machine.png)

**Model** is the machine to build: an Octane2, an Origin 200 and so on, with
its CPU count. The list follows the engine you picked. **Name** is the
machine's name and its directory name. Press Create. Creating a machine for
the first time downloads its PROM and checks it. It is the same step as
`origami create NAME --preset PRESET`; [Create a machine](usage.md#create-a-machine)
lists the presets.

## Start it

Select the machine and press Start (Ctrl+Shift+R; Cmd+R on macOS). The
machine's page shows a starting screen while the emulator launches. Until it
is up, the button reads Force Quit rather than Power Off.

![The starting screen](images/app/linux-starting.png)

When the guest draws, its screen replaces the page. This is an Octane2's PROM
menu.

![The PROM menu of a running Octane2](images/app/linux-running.png)

The picture goes from the emulator to your GPU without the CPU copying it
where the platform allows. If it cannot, the app copies it instead, and
[Diagnostics](#diagnostics) says why.

The picture fills the window at any size, keeping its shape. macOS and Windows
draw in-between sizes sharp: each guest pixel stays flat and blends only at its
edge. Linux still smooths sizes between whole steps above 1x.

Click the screen to type and use the mouse in the guest. The pointer button in
the toolbar picks how the mouse works. *Follow My Mouse* makes the guest's
pointer track yours while it is over the screen. *Capture on Click* takes the
mouse and keyboard when you click and hands them back on Ctrl+Alt (Linux).

Power Off asks the guest to stop. If the guest has not stopped after 30
seconds, the app offers Force Quit, which ends the emulator at once; the guest
does not get to write its disks. When you quit the app with machines running,
Keep Running leaves them going without it.

## The console

![The Console window showing the PROM's power-on messages](images/app/linux-console.png)

The Console button (Ctrl+Shift+O; Alt+Cmd+C on macOS) opens the machine's
serial console in its own window, one window per machine. It is a real
terminal, so IRIX's tools see colours, function keys and wide characters, and
it keeps 5000 lines of history. Each run starts with a clean console. On
Linux, select with the mouse and copy with Ctrl+Shift+C, paste with
Ctrl+Shift+V or Shift+Insert, and scroll back with the mouse wheel or
Shift+Page Up.

A machine whose console is set to a serial line prints its PROM and system
messages here; one whose console is graphics prints little. Set the line with
`origami nvram MACHINE set console=d`; see
[Which serial line is the console](usage.md#which-serial-line-is-the-console).
On a new machine's first boot the PROM warns about the NVRAM checksum and
reports failed disk diagnostics. Both are normal until an operating system is
installed.

## Pause and resume

![A paused machine, with the Resume button in the toolbar](images/app/linux-paused.png)

Pause (Ctrl+P on Linux and Windows; on macOS use the Machine menu) freezes
the machine in place. Its state reads Paused and the toolbar button becomes
Resume. Resume carries on where it left off.

## Screenshots

Choose Screenshot from the Machine menu while a machine runs (Save Screenshot,
Cmd+Shift+S, on macOS). The app saves the guest's screen as a PNG in your
Pictures folder, named like `octane2 2026-11-18 at 14.30.05.png`. It is the
guest's own picture, not a capture of your window. Linux shows a toast with a
Show in Files button. Windows puts the file name in the status bar with a
button that opens Explorer, and macOS announces the file and can reveal it in
Finder.

## Diagnostics

![The Diagnostics window for a running machine](images/app/linux-diagnostics.png)

Diagnostics (Ctrl+Shift+L; Alt+Cmd+L on macOS, or the row's right-click menu)
opens a text report for the selected machine: its engine, state, the last
error, each graphics head with its size and whether it has a picture, the
console's connection and how many frames the screen has received and shown.
Copy Report puts it on the clipboard. Attach it to a bug report.

When a machine stops unexpectedly, the report's Reason and Last error say why.

## The CPU panel

![The CPU panel under a running machine's picture](images/app/linux-cpu-panel.png)

The CPU button shows a bar for each emulated CPU under the picture, drawn in
the style of IRIX's `gr_osview`: a title and a coloured legend over a framed
bar cut into user (blue), sys (red) and spare (green), with a ruler marked in
tenths. The numbers are measured on your computer, not inside the guest, so
they show how hard the emulation works, not what IRIX reports. Spare is the
share of a host core the CPU's thread did not use. `gr_osview`'s interrupt,
wait, memory and graphics bars need a view inside the guest and are not shown.
The panel updates twice a second while it is shown and does no work when
hidden.

## Settings

![A machine's Settings window](images/app/linux-settings.png)

Settings (Ctrl+Shift+I; Cmd+I on macOS) edits the selected machine's file:
its name, memory and processor, graphics, drives, network, startup and input.
The Network page includes port forwards from the host to the guest. Startup
and firmware picks the console serial line, the same choice as
[`origami console-set`](usage.md#which-serial-line-is-the-console). A running
machine refuses edits; stop it first.

## About

![The About window](images/app/linux-about.png)

About shows the version, who made Origami and the project link. Its
Acknowledgements button lists what Origami is built on (QEMU with the link to
the source of the bundled build, iris, Ghostty's terminal library and
Instigator), the thanks, and each photograph and the artwork with its licence.
Its Licences button opens the full text of every licence bundled, grouped by
licence with the components under each, and a search box.

![The Acknowledgements window](images/app/linux-acknowledgements.png)

![The Licences window](images/app/linux-licences.png)

## The same from the command line

The same machines work from the CLI, on the same directories:

![origami machines, listing the presets](images/app/cli-machines.png)

![origami create and validate](images/app/cli-create.png)

![origami run in the background, then status and stop](images/app/cli-run.png)

`origami machines` lists the presets, `create` makes a machine, `run` starts
it and `status` and `stop` follow it. [Using Origami](usage.md) covers the
rest.
