# Security Policy

## Supported Versions

We actively maintain and provide updates for the latest release branch of the Toram Companion Framework.

| Version | Supported          |
| ------- | ------------------ |
| 0.96.x  | :white_check_mark: |
| < 0.96  | :x:                |

---

## Reporting a Vulnerability

The project team takes security and ethical software research seriously. If you discover a security vulnerability, buffer overflow, or potential privilege escalation issue within this codebase, please follow responsible disclosure practices.

### How to Report:
1. **Do NOT file a public issue** for zero-day vulnerabilities or security bugs that could put users at immediate risk.
2. Please submit a confidential report through **GitHub Security Advisories**:
   - Open this repository's **Security** tab → **Advisories** → **Report a vulnerability**.
   - Submit private details there.
3. Include the following details in your report:
   - Type of issue (e.g. out-of-bounds memory access, integer overflow, IPC race condition).
   - Component affected (`protocol`, `wcore`, `wboot`, or `companion`).
   - Step-by-step reproduction instructions or a minimal Proof of Concept (PoC).
   - Potential impact or exploitation scenario.

### Response Timeline
- We will acknowledge receipt of your vulnerability report within 48 hours.
- We will verify the issue and keep you informed of progress toward a fix.
- Once a fix is verified, a patched release will be published alongside public attribution (unless you prefer to remain anonymous).

---

## Scope and Research Disclaimer

This software is developed strictly for interoperability testing, educational exploration, and security research on memory architectures and IL2CPP runtimes. It is intended to be run exclusively in private testing environments owned and controlled by the operator.
