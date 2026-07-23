import "./style.css";

const header = document.querySelector("[data-header]");
const year = document.querySelector("[data-year]");
const windowsModal = document.querySelector("[data-windows-modal]");
const windowsOpenButtons = document.querySelectorAll("[data-windows-open]");
const windowsCloseButtons = document.querySelectorAll("[data-windows-close]");
const downloadLinks = document.querySelectorAll("[data-download]");
let lastFocusedElement = null;

if (year) year.textContent = new Date().getFullYear().toString();

const updateHeader = () => {
  header?.classList.toggle("scrolled", window.scrollY > 18);
};

updateHeader();
window.addEventListener("scroll", updateHeader, { passive: true });

const openWindowsModal = () => {
  if (!windowsModal) return;
  lastFocusedElement = document.activeElement;
  windowsModal.hidden = false;
  document.body.classList.add("modal-open");
  windowsModal.querySelector("[data-windows-close]")?.focus();
};

const closeWindowsModal = () => {
  if (!windowsModal || windowsModal.hidden) return;
  windowsModal.hidden = true;
  document.body.classList.remove("modal-open");
  lastFocusedElement?.focus?.();
};

windowsOpenButtons.forEach((button) => button.addEventListener("click", openWindowsModal));
windowsCloseButtons.forEach((button) => button.addEventListener("click", closeWindowsModal));
windowsModal?.addEventListener("click", (event) => {
  if (event.target === windowsModal) closeWindowsModal();
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeWindowsModal();
});

const resolveLatestDownloads = async () => {
  try {
    const response = await fetch("https://api.github.com/repos/GnosysLabs/noise/releases/latest", {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!response.ok) return;
    const release = await response.json();
    const assets = Array.isArray(release.assets) ? release.assets : [];
    const mac = assets.find((asset) => /macOS-arm64\.zip$/i.test(asset.name));
    const windows = assets.find((asset) => /x64-setup\.exe$/i.test(asset.name));

    downloadLinks.forEach((link) => {
      const platform = link.dataset.download;
      if (platform === "mac" && mac?.browser_download_url) link.href = mac.browser_download_url;
      if (platform === "windows" && windows?.browser_download_url) link.href = windows.browser_download_url;
    });
  } catch {
    // The release page remains a safe fallback when GitHub's API is unavailable.
  }
};

resolveLatestDownloads();
