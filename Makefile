ENV ?= dev
LOCALDEV_CLUSTER ?= epa
VERSION ?= $(shell git describe --tags --always --dirty)

KUBECTL ?= kubectl
KUSTOMIZE ?= kustomize
CARGO ?= cargo

DUMMY_METRICS_IMAGE ?= ctxsh/dummy-metrics
DUMMY_METRICS_TAG ?= v0.0.2

###
### Build targets
###
.PHONY: build
build:
	$(CARGO) build --release

.PHONY: test
test:
	$(CARGO) test

.PHONY: clippy
clippy:
	$(CARGO) clippy --all-targets --all-features

.PHONY: fmt
fmt:
	$(CARGO) fmt

.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --check


###
### Docker targets
###
.PHONY: dummy-metrics
dummy-metrics:
	docker buildx build \
		--platform linux/amd64,linux/arm64 \
		-t $(DUMMY_METRICS_IMAGE):$(DUMMY_METRICS_TAG) \
		-t $(DUMMY_METRICS_IMAGE):latest \
		--push \
		examples/dummy-metrics

###
### Local development
###
.PHONY: localdev
localdev: localdev-cluster localdev-install

.PHONY: localdev-cluster
localdev-cluster:
	@if k3d cluster get $(LOCALDEV_CLUSTER) --no-headers >/dev/null 2>&1; \
		then echo "Cluster '$(LOCALDEV_CLUSTER)' already exists"; \
		else echo "Creating k3d cluster '$(LOCALDEV_CLUSTER)'..." && \
		k3d cluster create --config config/k3d/config.yaml --volume $(PWD):/app; \
	fi

.PHONY: localdev-cert-manager
localdev-cert-manager:
	@echo "Installing cert-manager..."
	@$(KUBECTL) apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.15.3/cert-manager.yaml
	@echo "Waiting for cert-manager to be ready..."
	@$(KUBECTL) wait --for=condition=available --timeout=120s deploy -l app.kubernetes.io/instance=cert-manager -n cert-manager

.PHONY: localdev-install
localdev-install: localdev-cert-manager
	@echo "Installing EPA controller..."
	@$(KUSTOMIZE) build config/epa/overlays/$(ENV) | $(KUBECTL) apply -f -
	@echo "Waiting for EPA controller to be ready..."
	@$(KUBECTL) wait --for=condition=available --timeout=120s deploy/epa-controller -n epa-system

.PHONY: localdev-clean
localdev-clean:
	@echo "Deleting k3d cluster '$(LOCALDEV_CLUSTER)'..."
	@k3d cluster delete $(LOCALDEV_CLUSTER)

.PHONY: localdev-restart
localdev-restart: localdev-clean localdev

.PHONY: localdev-uninstall
localdev-uninstall:
	@echo "Uninstalling EPA controller..."
	@$(KUBECTL) delete -k config/epa/overlays/$(ENV)

###
### Dev pod operations
###
.PHONY: run
run:
	$(eval POD := $(shell kubectl get pods -n epa-system -l app=epa-controller -o=custom-columns=:metadata.name --no-headers))
	@echo "Running controller in pod $(POD)..."
	@$(KUBECTL) exec -n epa-system -it pod/$(POD) -- bash -c "cargo run --bin external-pod-autoscaler"

.PHONY: exec
exec:
	$(eval POD := $(shell kubectl get pods -n epa-system -l app=epa-controller -o=custom-columns=:metadata.name --no-headers))
	@echo "Connecting to pod $(POD)..."
	@$(KUBECTL) exec -n epa-system -it pod/$(POD) -- bash

.PHONY: logs
logs:
	@$(KUBECTL) logs -n epa-system -l app=epa-controller -f

###
### Cluster management
###
.PHONY: cluster-info
cluster-info:
	@k3d cluster list
	@echo ""
	@$(KUBECTL) cluster-info
	@echo ""
	@$(KUBECTL) get nodes

.PHONY: cluster-stop
cluster-stop:
	@k3d cluster stop $(LOCALDEV_CLUSTER)

.PHONY: cluster-start
cluster-start:
	@k3d cluster start $(LOCALDEV_CLUSTER)

###
### Help
###
.PHONY: help
help:
	@echo "External Pod Autoscaler Makefile"
	@echo ""
	@echo "Build targets:"
	@echo "  build              - Build release binary"
	@echo "  test               - Run tests"
	@echo "  clippy             - Run clippy linter"
	@echo "  fmt                - Format code"
	@echo "  fmt-check          - Check code formatting"
	@echo ""
	@echo "Docker targets:"
	@echo "  dummy-metrics - Build and push multi-arch dummy-metrics image"
	@echo ""
	@echo "Local development:"
	@echo "  localdev           - Create k3d cluster + install controller"
	@echo "  localdev-cluster   - Create k3d cluster only"
	@echo "  localdev-cert-manager - Install cert-manager"
	@echo "  localdev-install   - Install controller in cluster"
	@echo "  localdev-uninstall - Uninstall controller"
	@echo "  localdev-clean     - Delete k3d cluster"
	@echo "  localdev-restart   - Delete and recreate cluster"
	@echo ""
	@echo "Dev pod operations:"
	@echo "  run                - Run controller in dev pod"
	@echo "  exec               - Shell into dev pod"
	@echo "  logs               - Tail controller logs"
	@echo ""
	@echo "Cluster management:"
	@echo "  cluster-info       - Show cluster information"
	@echo "  cluster-stop       - Stop k3d cluster"
	@echo "  cluster-start      - Start k3d cluster"
	@echo ""
	@echo "Environment variables:"
	@echo "  LOCALDEV_CLUSTER   - Cluster name (default: epa)"
	@echo "  ENV                - Environment (default: dev)"
