<div align="center">
  <a href="README.md"><b>Burnout</b></a>
</div>

# Security Policy

## Supported versions

Burnout is in the planning stage and has no release.
When it has one, fixes go to the latest version on the default branch.
Older versions are not maintained.

## Reporting a vulnerability

Report security problems privately, and not through a public issue.

- Preferred: open a private security advisory with the "Report a vulnerability"
  button on the Security tab of this repository.
- Alternative: email the maintainer at stivenagostingjekaj@gmail.com.

Include the steps to reproduce, the affected commit, and the impact as you
understand it.
You can expect a first answer within a few days.
Your report gets an acknowledgement when the fix ships, unless you prefer to
stay anonymous.

## The threat model

Burnout has an unusual shape, so read this before you report.

**The Software runs with high privilege and writes to raw devices.**
A tool that writes a block device needs administrator rights on Windows, and
root rights on macOS and on Linux.
That is the job, and it is not a defect.
The question that matters is what the Software does with those rights.

**The worst outcome is a write to the wrong device.**
A bug that selects, keeps, or resolves the wrong target destroys a disk that
the person did not choose.
This is the highest severity problem that this project can have.
It outranks a crash, and it outranks a slow write.

**A system disk is never a target.**
The Software refuses to write to the disk that the running system starts from,
and to any disk that it cannot prove is removable when the person did not force
the choice.
A way around that refusal is a vulnerability, not a feature request.

**The input is untrusted.**
An ISO file comes from the internet.
A parser for a file system, a boot sector, or an archive inside that image
reads bytes that an attacker chooses.

**These are in scope:**

- Any path that writes to a device that the person did not select and confirm.
- Any way to defeat the refusal to write to a system disk or to a fixed disk.
- A parser that a crafted image can crash, hang, or make read or write outside
  its buffer.
- A component that the Software downloads without a check of its signature or
  its hash, or a check that a network attacker can pass.
- A privilege escalation, such as a helper that a normal user can drive to do
  something the user cannot do alone.
- A verification pass that reports success for a write that did not match the
  source.
- A temporary file, a mount point, or a log that leaks a credential, or that a
  local attacker can replace between the check and the use.

**These are out of scope:**

- A report that the Software needs administrator or root rights. It writes
  block devices, so it does.
- A report that the Software erases a drive. That is what it does, and
  [TERMS.md](TERMS.md) section 4 says so.
- A report that an installer option removes a hardware check. That option is
  documented, the person asks for it, and it defeats no licence or copy
  protection measure.
- A drive that does not start after a write. That is a bug, so open a normal
  issue.
- Findings from an automated scanner with no working demonstration.
