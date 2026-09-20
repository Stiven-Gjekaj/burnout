<div align="center">
  <a href="README.md"><b>Burnout</b></a>
</div>

# Terms and Conditions

These terms govern your use of Burnout, an open source command line tool that
writes a disk image to a drive (the "Software").
By using, building, running, or contributing to the Software, you agree to
these terms.
If you do not agree, do not use the Software.

## 1. Licence

The Software is licensed under the MIT License, in the [LICENSE](LICENSE) file.
That licence is the authoritative statement of your rights to use, copy,
modify, and distribute the Software.
If anything here conflicts with the LICENSE file, the LICENSE file governs.

## 2. No warranty

The Software is provided "as is", without warranty of any kind, express or
implied.
This includes the warranties of merchantability, fitness for a particular
purpose, and noninfringement.
You use the Software at your own risk.

## 3. Limitation of liability

To the maximum extent that the law permits, the authors and copyright holders
are not liable for any claim, damages, or other liability that arises from the
use of the Software.
This applies whether the claim is in contract, in tort, or otherwise.

## 4. The Software destroys data

**Read this section before you run the Software.**

The Software writes directly to a block device.
A write erases everything on the target drive.
The erased data does not go to a recycle bin, and no undo exists.

The Software asks you to confirm the target.
That confirmation is the last check, and it is yours to make.
The Software cannot know which drive holds the only copy of your photographs.

You are responsible for the target that you select.
Keep a backup of anything that you value before you start.
A wrong target, a drive that fails during the write, and a cable that comes
loose all give the same result, and none of them is recoverable.

## 5. Operating system images and licences

The Software writes an image that you supply.
It does not supply, download, host, or distribute any operating system.

You must hold a valid licence for any operating system that you install with a
drive that the Software makes.
A Windows installation needs a licence from Microsoft.
The terms of that licence come from Microsoft, not from this project.

## 6. Compatibility options

The Software can change an installer so that it starts on hardware that the
installer refuses by default.
An example is the removal of a hardware check from a Windows 11 installer.

These options exist for hardware that you own.
They change the installer that you create, and they do not defeat any licence
check, activation step, or copy protection measure.
The maker of the operating system does not support an installation of this
kind, and may refuse to give updates for it.
You accept that outcome when you use the option.

Do not use the Software, or any option in it, to break the law where you are.

## 7. Contributions

If you contribute to the Software, you agree that your contribution carries the
same MIT Licence as the rest of the project.
See [CONTRIBUTING.md](CONTRIBUTING.md) for how to take part.

## 8. Third-party components

The Software uses open source libraries, and it can use boot loaders and
similar components that other people write.
Each of those carries its own licence, and that licence governs that component.
The [README](README.md) names the current choices.
A component that the Software downloads at run time comes from its own
publisher, under its own terms.

## 9. Project name

"Burnout" and the `burnout` command name identify this project.
You may refer to the project by name.
Please do not use the name in a way that suggests endorsement of, or connection
with, a modified or unofficial version, without permission.

## 10. Changes to these terms

These terms may change as the project grows.
The version in the default branch of the repository is the current one.
If you continue to use the Software after a change, you accept the updated
terms.
