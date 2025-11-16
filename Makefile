# Makefile

.PHONY: serve build clean help

# Default target
.DEFAULT_GOAL := help

##@ Documentation

serve: ## Run MkDocs development server with uvx
	uvx --with mkdocs-material mkdocs serve --dirty -a 0.0.0.0:65533 -o


build: ## Build the documentation site
	uvx --with mkdocs-material mkdocs build


# publish: build ## publish documentation on github
#	uvx --with mkdocs-material mkdocs gh-deploy --force


clean: ## Clean the site directory
	rm -rf site/


help: ## Show help message
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage:\n  make \033[36m<target>\033[0m\n"} /^[a-zA-Z_0-9-]+:.*?##/ { printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2 } /^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) } ' $(MAKEFILE_LIST)
