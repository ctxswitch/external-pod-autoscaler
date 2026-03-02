//! External Pod Autoscaler for Kubernetes
//!
//! A Kubernetes controller and metrics aggregation system for the ExternalPodAutoscaler (EPA)
//! custom resource. Enables horizontal pod autoscaling based on custom external metrics
//! scraped from application endpoints.
//!
//! # Architecture
//!
//! The system consists of three main components:
//!
//! - **Controller**: Watches EPA resources and manages corresponding HorizontalPodAutoscaler (HPA) resources
//! - **Scraper**: Polls application metrics endpoints and stores time-windowed samples
//! - **Webhook**: Provides validation, mutation, and external metrics API for the HPA controller

/// Kubernetes API types for ExternalPodAutoscaler CRD
pub mod apis;

/// Controller for managing ExternalPodAutoscaler resources
pub mod controller;

/// Metrics scraping service for collecting data from application pods
pub mod scraper;

/// Time-windowed metrics storage for aggregation
pub mod store;

/// Webhook server for validation, mutation, and external metrics API
pub mod webhook;
