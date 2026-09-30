# Contributing to Origami

Open issues and pull requests on GitHub against `main`. Accepted changes retain
contributor authorship. The maintainer closes the pull request with a link to
the resulting commit.

Include the Origami version, host operating system, machine configuration,
steps to reproduce and relevant error output. Keep firmware, IRIX media,
proprietary drivers and snapshots out of commits and uploads. A small synthetic
reproduction is preferable when it can demonstrate the problem.

Change emulator behavior and tests in the QEMU repository, and installer behavior
and tests in Instigator. Submit each dependency change to its own repository.
The product updates its submodule pin after that change is accepted.

Origami is an experimental hobby project developed largely with AI assistance.
Incomplete emulation and failing guest configurations are expected. Describe what
was tested and any known limits of a proposed change. Original product code is
BSD-3-Clause. QEMU, Instigator and third-party components retain their own licenses.
