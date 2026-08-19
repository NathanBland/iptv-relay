# Third-party runtime notices

IPTV Gateway's original source code is licensed under the MIT License.

The deployment image installs FFmpeg and VLC from the Debian distribution. Those programs are
independent works distributed under their respective licenses. The image build must retain the
package-provided copyright and license files. A release pipeline must generate an SBOM and attach
the exact package versions and corresponding source-package locations before publishing images.

The project uses open-source Rust and JavaScript dependencies. Their licenses must be checked by
the repository's dependency audit before a release is considered complete.

