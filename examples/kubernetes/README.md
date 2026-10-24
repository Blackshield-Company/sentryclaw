# Kubernetes Configuration Example

This example provides Kubernetes manifests for deploying SentryClaw.

## Namespace

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: sentryclaw
```

## ConfigMap

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: sentryclaw-config
  namespace: sentryclaw
data:
  config.toml: |
    [server]
    host = "0.0.0.0"
    port = 3000

    [github]
    webhook_secret = "${GITHUB_WEBHOOK_SECRET}"
    app_id = "${GITHUB_APP_ID}"
    private_key_path = "/etc/sentryclaw/github-private-key.pem"
    use_app_auth = true
    installation_id = ${GITHUB_INSTALLATION_ID}

    [llm]
    provider = "llamacpp"
    base_url = "http://llama:8080"
    model = "codellama-34b.Q4_K_M"
    max_tokens = 4096
    temperature = 0.1

    [database]
    path = "/data/sentryclaw.db"

    [dashboard]
    enabled = true
```

## Secret

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: sentryclaw-secrets
  namespace: sentryclaw
type: Opaque
stringData:
  GITHUB_WEBHOOK_SECRET: "your-webhook-secret"
  GITHUB_APP_ID: "123456"
  GITHUB_INSTALLATION_ID: "12345678"
```

## Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: sentryclaw
  namespace: sentryclaw
  labels:
    app: sentryclaw
spec:
  replicas: 1
  selector:
    matchLabels:
      app: sentryclaw
  template:
    metadata:
      labels:
        app: sentryclaw
    spec:
      containers:
        - name: sentryclaw
          image: ghcr.io/blackshield-company/sentryclaw:latest
          ports:
            - containerPort: 3000
              name: http
          env:
            - name: CONFIG_PATH
              value: "/app/config.toml"
            - name: RUST_LOG
              value: "info"
            - name: GITHUB_WEBHOOK_SECRET
              valueFrom:
                secretKeyRef:
                  name: sentryclaw-secrets
                  key: GITHUB_WEBHOOK_SECRET
          volumeMounts:
            - name: config
              mountPath: /app/config.toml
              subPath: config.toml
            - name: data
              mountPath: /data
            - name: github-key
              mountPath: /etc/sentryclaw
              readOnly: true
          resources:
            requests:
              memory: "64Mi"
              cpu: "100m"
            limits:
              memory: "256Mi"
              cpu: "500m"
          livenessProbe:
            httpGet:
              path: /health
              port: 3000
            initialDelaySeconds: 10
            periodSeconds: 30
          readinessProbe:
            httpGet:
              path: /health
              port: 3000
            initialDelaySeconds: 5
            periodSeconds: 10
      volumes:
        - name: config
          configMap:
            name: sentryclaw-config
        - name: data
          persistentVolumeClaim:
            claimName: sentryclaw-data
        - name: github-key
          secret:
            secretName: sentryclaw-github-key
            items:
              - key: private-key.pem
                path: github-private-key.pem
```

## Service

```yaml
apiVersion: v1
kind: Service
metadata:
  name: sentryclaw
  namespace: sentryclaw
spec:
  selector:
    app: sentryclaw
  ports:
    - protocol: TCP
      port: 80
      targetPort: 3000
  type: ClusterIP
```

## Persistent Volume Claim

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: sentryclaw-data
  namespace: sentryclaw
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi
```

## Horizontal Pod Autoscaler

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: sentryclaw
  namespace: sentryclaw
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: sentryclaw
  minReplicas: 1
  maxReplicas: 3
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
```

## Ingress

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: sentryclaw
  namespace: sentryclaw
  annotations:
    cert-manager.io/cluster-issuer: "letsencrypt-prod"
spec:
  tls:
    - hosts:
        - sentryclaw.example.com
      secretName: sentryclaw-tls
  rules:
    - host: sentryclaw.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: sentryclaw
                port:
                  number: 80
```

## Usage

```bash
# Apply all manifests
kubectl apply -f namespace.yaml
kubectl apply -f configmap.yaml
kubectl apply -f secret.yaml
kubectl apply -f pvc.yaml
kubectl apply -f deployment.yaml
kubectl apply -f service.yaml
kubectl apply -f hpa.yaml
kubectl apply -f ingress.yaml

# Check status
kubectl get pods -n sentryclaw
kubectl logs -f deployment/sentryclaw -n sentryclaw
```
