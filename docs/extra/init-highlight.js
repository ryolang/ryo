// Use the official MkDocs Material hook
document$.subscribe(() => {
  // First, register the Ryo language definition if it hasn't been already.
  // The 'ryo' function comes from the ryo-highlight.js script.
  if (!hljs.getLanguage("ryo")) {
    hljs.registerLanguage("ryo", ryo);
  }

  // Now, find all Ryo code blocks and highlight them.
  // This will run every time a new page is loaded via instant navigation.
  document.querySelectorAll("pre code.language-ryo").forEach((block) => {
    hljs.highlightElement(block);
  });
});
