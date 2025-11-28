// Use the official MkDocs Material hook
document$.subscribe(() => {
  // First, register the Ryo language definition if it hasn't been already.
  // The 'ryo' function comes from the ryo-highlight.js script.
  if (!hljs.getLanguage("ryo")) {
    hljs.registerLanguage("ryo", ryo);
  }

  // Now, highlight all code blocks (all languages).
  // This will run every time a new page is loaded via instant navigation.
  document.querySelectorAll("pre code").forEach((block) => {
    hljs.highlightElement(block);
  });
});
