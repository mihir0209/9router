# 9Router Kubernetes Production Deployment

Production-grade Kubernetes manifests for deploying 9Router in high-availability clustered environments.

## Manifest Architecture

| File | Resource | Description |
|---|---|---|
| `namespace.yaml` | `Namespace` | Dedicated `9router` namespace for multi-tenant isolation |
| `configmap.yaml` | `ConfigMap` | Non-sensitive runtime configuration (ports, logs, model source, DB pool) |
| `secret.yaml` | `Secret` | Sensitive credentials (tokens, salts, passwords, DB credentials) |
| `deployment.yaml` | `Deployment` | 2+ replicas with rolling updates, distroless nonroot security, resource limits, and health probes |
| `service-clusterip.yaml` | `Service` (ClusterIP) | Internal east-west cluster networking on port 20127 |
| `service-loadbalancer.yaml` | `Service` (LoadBalancer) | External north-south ingress/edge load balancing on port 20127 |
| `hpa.yaml` | `HorizontalPodAutoscaler` | Horizontal autoscaling between 2-10 pods targeting 70% CPU utilization |
| `kustomization.yaml` | `Kustomization` | Standard Kustomize overlay file for one-command deployment |

## Resource Specifications & Sizing

The deployment adheres to the 9Router production sizing profile:
- **CPU Request**: `100m` (0.1 core)
- **CPU Limit**: `500m` (0.5 core)
- **Memory Request**: `64Mi`
- **Memory Limit**: `256Mi`
- **Autoscaling Target**: 70% average CPU utilization across pods

## Health & Probes Contract

- **Startup Probe**: HTTP GET `/health` on port 20127 (failure threshold 15, period 2s)
- **Liveness Probe**: HTTP GET `/health` on port 20127 (initial delay 5s, period 10s, timeout 3s)
- **Readiness Probe**: HTTP GET `/health` on port 20127 (initial delay 3s, period 5s, timeout 2s)

Both `/health` and `/api/health` return HTTP `200 OK` with `{"ok":true}` without requiring an authorization header.

## Quick Start Deployment

### 1. Configure Secrets
Review and edit [secret.yaml](file:///home/ghostrider/.gemini/antigravity/worktrees/d6bd882f-9161-40f7-b8fb-24810377c66b/swarm-container-k8s-9e722acc/deploy/k8s/secret.yaml) to supply your JWT secret, admin token, and Postgres `DATABASE_URL`.

### 2. Apply with Kustomize
```bash
kubectl apply -k deploy/k8s/
```

### 3. Verify Rollout Status
```bash
kubectl rollout status deployment/9router -n 9router
kubectl get pods,svc,hpa -n 9router
```

### 4. Test Connectivity
```bash
# Port-forward local port 20127 to the cluster service
kubectl port-forward svc/9router-clusterip 20127:20127 -n 9router

# Run health check
curl http://127.0.0.1:20127/api/health
```
