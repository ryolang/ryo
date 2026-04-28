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

    const CACHE_KEY = "gh-star-count";
    const CACHE_TTL_MS = 60 * 60 * 1000; // 1h — avoids the 60/hr anon API limit
    const format = (n) => (n > 999 ? (n / 1000).toFixed(1) + "k" : String(n));
    const render = (n) => {
        countEl.textContent = format(n);
        countEl.hidden = false;
    };

    try {
        const raw = localStorage.getItem(CACHE_KEY);
        if (raw) {
            const { n, t } = JSON.parse(raw);
            if (typeof n === "number" && Date.now() - t < CACHE_TTL_MS) {
                render(n);
                return;
            }
        }
    } catch {}

    fetch("https://api.github.com/repos/ryolang/ryo")
        .then((res) => res.json())
        .then((data) => {
            if (typeof data.stargazers_count === "number") {
                render(data.stargazers_count);
                try {
                    localStorage.setItem(
                        CACHE_KEY,
                        JSON.stringify({ n: data.stargazers_count, t: Date.now() }),
                    );
                } catch {}
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
