// ----- Theme toggle -----
(function () {
    const root = document.documentElement;
    const btn = document.getElementById("theme-toggle");
    if (!btn) return;

    btn.addEventListener("click", () => {
        const isDark = root.classList.toggle("dark");
        localStorage.setItem("theme", isDark ? "dark" : "light");
    });

    // Follow system theme if user hasn't explicitly chosen
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", (e) => {
        if (localStorage.getItem("theme")) return;
        root.classList.toggle("dark", e.matches);
    });
})();

// ----- GitHub stars -----
(function () {
    const countEl = document.getElementById("gh-star-count");
    if (!countEl) return;

    fetch("https://api.github.com/repos/ryolang/ryo")
        .then((res) => res.json())
        .then((data) => {
            if (typeof data.stargazers_count === "number") {
                const n = data.stargazers_count;
                countEl.textContent = n > 999 ? (n / 1000).toFixed(1) + "k" : String(n);
                countEl.hidden = false;
            }
        })
        .catch((err) => console.error(err));
})();

// ----- Copy buttons -----
(function () {
    document.querySelectorAll(".copy-btn").forEach((btn) => {
        btn.addEventListener("click", () => {
            let text = btn.dataset.copy;
            if (!text && btn.dataset.copyTarget) {
                const target = document.getElementById(btn.dataset.copyTarget);
                if (target) text = target.innerText;
            }
            if (!text) return;
            navigator.clipboard.writeText(text).then(() => {
                btn.classList.add("copied");
                setTimeout(() => btn.classList.remove("copied"), 2000);
            });
        });
    });
})();
