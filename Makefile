LOCALDEV_CLUSTER ?= epa
RELEASE_NAME ?= epa
RELEASE_NAMESPACE ?= epa-system
VERSION ?= $(shell git describe --tags --always --dirty)

KUBECTL ?= kubectl
HELM ?= helm
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
		k3d cluster create --config cluster.yaml --volume $(PWD):/app; \
	fi

.PHONY: localdev-install
localdev-install:
	@echo "Installing EPA controller..."
	@$(HELM) dependency build charts/external-pod-autoscaler
	@$(HELM) upgrade --install $(RELEASE_NAME) charts/external-pod-autoscaler \
		--namespace $(RELEASE_NAMESPACE) --create-namespace \
		-f charts/external-pod-autoscaler/values.localdev.yaml
	@echo "Waiting for EPA controller to be ready..."
	@$(KUBECTL) wait --for=condition=available --timeout=120s deploy/$(RELEASE_NAME)-external-pod-autoscaler -n $(RELEASE_NAMESPACE)

.PHONY: localdev-clean
localdev-clean:
	@echo "Deleting k3d cluster '$(LOCALDEV_CLUSTER)'..."
	@k3d cluster delete $(LOCALDEV_CLUSTER)

.PHONY: localdev-restart
localdev-restart: localdev-clean localdev

.PHONY: localdev-examples
localdev-examples:
	@echo "Applying examples..."
	@$(KUBECTL) apply -f examples/dummy-metrics.yaml
	@$(KUBECTL) apply -f examples/epa.yaml

.PHONY: localdev-uninstall
localdev-uninstall:
	@echo "Uninstalling EPA controller..."
	@$(HELM) uninstall $(RELEASE_NAME) --namespace $(RELEASE_NAMESPACE)
	@$(KUBECTL) delete -f examples/epa.yaml --ignore-not-found
	@$(KUBECTL) delete -f examples/dummy-metrics.yaml --ignore-not-found

###
### Dev pod operations
###
.PHONY: run
run:
	$(eval POD := $(shell $(KUBECTL) get pods -n $(RELEASE_NAMESPACE) -l app.kubernetes.io/name=external-pod-autoscaler -o=custom-columns=:metadata.name --no-headers))
	@echo "Running controller in pod $(POD)..."
	@$(KUBECTL) exec -n $(RELEASE_NAMESPACE) -it pod/$(POD) -- bash -c "cargo run --bin external-pod-autoscaler"

.PHONY: exec
exec:
	$(eval POD := $(shell $(KUBECTL) get pods -n $(RELEASE_NAMESPACE) -l app.kubernetes.io/name=external-pod-autoscaler -o=custom-columns=:metadata.name --no-headers))
	@echo "Connecting to pod $(POD)..."
	@$(KUBECTL) exec -n $(RELEASE_NAMESPACE) -it pod/$(POD) -- bash

.PHONY: logs
logs:
	@$(KUBECTL) logs -n $(RELEASE_NAMESPACE) -l app.kubernetes.io/name=external-pod-autoscaler -f

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
	@echo "  localdev-install   - Install controller via Helm"
	@echo "  localdev-examples  - Apply example resources"
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
	@echo "  RELEASE_NAME       - Helm release name (default: epa)"
	@echo "  RELEASE_NAMESPACE  - Helm release namespace (default: epa-system)"
