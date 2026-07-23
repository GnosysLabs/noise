import "./style.css";

const header = document.querySelector("[data-header]");
const year = document.querySelector("[data-year]");

if (year) year.textContent = new Date().getFullYear().toString();

const updateHeader = () => {
  header?.classList.toggle("scrolled", window.scrollY > 18);
};

updateHeader();
window.addEventListener("scroll", updateHeader, { passive: true });
