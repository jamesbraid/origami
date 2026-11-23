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

On Windows you can instead run `origami-<version>-windows-x86_64-setup.exe` or
`origami-<version>-windows-arm64-setup.exe`. It installs for you alone under
`%LocalAppData%\Programs\Origami` without asking for administrator rights (the
first page offers all users), adds Origami to the Start menu and the `origami`
command to PATH (untick the box to skip that), and replaces an older install in
place. Uninstall it from Settings, Apps; your machine library is never touched.

Builds that are not signed, such as one from a fork or a development build,
are blocked by macOS Gatekeeper and flagged by Windows SmartScreen. On macOS,
run `xattr -dr com.apple.quarantine Origami.app` once after extracting. On
Windows, choose More info, then Run anyway. Signed releases need neither.

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

The Machines list is on the left, one row per machine with its state. Selecting a
row shows the machine on the right: its picture, name and a Start button, with
Memory, Nodes and Topology below. The machine's state is shown in the title bar
or toolbar and in its row, and nowhere else. Double-click a row, or use
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

**Disk** is the machine's system disk: no disk, an empty disk of the size you
give (4 GB to start with), or an image you already have (CHD, qcow2 or raw).
Use a disk made for the same kind of machine. An image is copied into the
machine's folder, which keeps your original safe and the machine in one
place, or used where it is. A CHD keeps its changes apart from your file
unless you choose to save into the image. A copy shows its progress and can be
cancelled; a cancelled copy leaves the machine without a disk. The sheet
warns when another machine already uses the file. Each disk shows a badge
naming what is on it, such as "IRIX 6.5.30" or "OpenBSD 6.9", or "Unknown";
the library row shows the system disk's badge. This is
[`drive-create`](usage.md) and `drive-attach` from the command line.

When the disk server lists a ready-made, boot-checked IRIX disk for the model
you picked, **Ready-made disk** joins the choices, with a list of IRIX
releases and their sizes. Create downloads the disk once, checks its SHA-256
and copies it into the machine's folder with its changes kept apart; the
download shows its progress and can be cancelled, and a stopped download
resumes; one that gets no data for a minute stops with a message to try
again. Cancelling the download makes no machine and keeps the part already
fetched; cancelling the copy that follows leaves the machine without a disk.
The disk must fit the model and both the download folder and the library need
room before it starts. Downloads are kept in one shared folder in the cache directory, with
no size limit and no automatic cleanup, so a second machine starts from the
same file without downloading again. Preferences, Storage lists them with a
Remove button, which is refused while a disk is being downloaded or copied.
The machine gets its own copy because the CHD driver keeps its
changes beside the image file and offers no way to put them elsewhere.

**Advanced** holds the machine ID and the Ethernet address. The ID is the
lowest free number from 1 to 2999 and makes the serial number, host id and
Ethernet address, so two machines in one folder never share them. Choose
another number, or give an address of your own for a machine that will sit on
your network. The sheet says why a number or address cannot be used. It is
`origami create --emu-id` and `--mac`.

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
its name, memory and processor, graphics, drives, network, shared folders, startup and input.
The Network page picks how the machine connects: Shared (NAT), Private,
Bridged or Off. Bridged gives the machine its own address on your network
through a tap device on Linux and Windows, or socket_vmnet on macOS; the page
says how to set that up and offers the commands to copy. It also shows the
machine's Ethernet address, which you can copy but not change, and the private
subnet under Advanced. After a change that moves the machine to a different
network, the page shows the command that makes the guest forget its old
address. This is [`origami network-set`](usage.md#join-a-real-network). The
page also holds port forwards from the host to the guest. Shared Folders
shares a folder of this computer with the guest over NFS: pick the folder, give it a
name, and the page shows the two commands to run as root in IRIX to mount it,
with Copy. This is [`origami share`](usage.md#share-a-host-folder). Startup
and firmware picks the console serial line, the same choice as
[`origami console-set`](usage.md#which-serial-line-is-the-console). A running
machine refuses edits; stop it first, except for the CD-ROM. The Drives page
adds a disk image and inserts a CD-ROM image. Change CD and Eject work while
the machine runs.

## Install IRIX

Machine > Install IRIX… opens the install assistant for a stopped machine.
New Machine can open it for you: with an empty disk chosen, tick Install IRIX
after creating. It is the app's version of
[Install IRIX](usage.md#install-irix) from the command line. The assistant
gets everything ready, serves the discs and lists each step with the
machine's own release and addresses in it.

Choose the release and profile (IRIX 6.5.30 Standard or Minimal, or 6.5.22),
then where the discs come from: downloaded from the Origami server while the
install runs, or a folder of disc images you already have, which the
assistant checks as soon as you choose it. Tick the software to add after
IRIX, such as MIPSpro, or add a package from a file or an HTTPS address. The
Disk step creates a system disk when the machine has none and asks before it
erases one it has. Check switches the machine to the private installation
network and checks every disc; Check thoroughly also reads the downloaded
ones.

On a machine whose PROM prints on a serial line, a Method step asks how the
install should run. Automatic, the default, types the package commands for
you: at `Inst>` press Install and Origami loads the install script, restarts
the machine, logs in as root and runs `halt -p`, while the Console shows the
guest and is read-only. It types nothing unless the console ends at `Inst>`.
If a conflict or a failed command stops it, the message says so and the guest
waits at `Inst>`: fix it in the Console and press "I fixed it at Inst>" to
finish by hand. If the packages went in but the login or `halt -p` failed,
use the Console to halt IRIX; Continue turns on once the machine has stopped.
Guided shows the commands for you to type. A machine with graphics keeps its
screen as the console, so it is always Guided, and the assistant never
switches a machine's console.

The next three steps start the media server and the machine, then list what
to do: label the disk, start the installer and install the packages. Copy
puts a command on the clipboard. On a machine whose console is a serial line,
Type It sends it to the console for you. A machine with graphics keeps its
screen as the console and is never switched to serial. Show Machine and Open
Console bring the machine and its console forward. The footer shows what the
media server is serving, and turns red with its message and a Restart button
if the server stops. Cancel stops the media server, and in the middle of the
install asks whether to stop the machine too. Quitting the app while the
server runs asks first, because the server stops with the app.

When the machine has stopped after `halt -p`, Finish puts the network back as
it was before the install, stops the media server and closes the assistant.
Closing earlier offers the same on a stopped machine, and a machine left on
the installation network offers it the next time you open the assistant.
Reopening on a machine that is already set up starts at the Disk step, with
Start over to begin again. Install IRIX is for machines that run IRIX.

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
