resource "aws_ecr_repository" "backend" {
  name                 = "${local.project}-${local.env}-backend"
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "AES256"
  }

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-backend"
  })
}

resource "aws_cloudwatch_log_group" "backend" {
  name              = "/aws/ecs/${local.project}-${local.env}-backend"
  retention_in_days = var.backend_log_retention_days
  tags              = local.tags
}

resource "aws_ecs_cluster" "backend" {
  name = "${local.project}-${local.env}-backend"

  setting {
    name  = "containerInsights"
    value = "enabled"
  }

  tags = local.tags
}

resource "aws_lb" "backend" {
  name               = "${local.project}-${local.env}-backend"
  internal           = false
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = [for subnet in aws_subnet.public : subnet.id]
  idle_timeout       = var.backend_alb_idle_timeout_seconds

  enable_deletion_protection = true

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-backend-alb"
  })
}

resource "aws_lb_target_group" "backend" {
  name        = "${local.project}-${local.env}-backend"
  port        = var.backend_container_port
  protocol    = "HTTP"
  target_type = "ip"
  vpc_id      = aws_vpc.main.id

  deregistration_delay = 30

  health_check {
    enabled             = true
    path                = var.backend_health_check_path
    matcher             = "200-399"
    interval            = 30
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 3
  }

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-backend-tg"
  })
}

resource "aws_lb_listener" "backend_http" {
  count = var.backend_enable_https ? 0 : 1

  load_balancer_arn = aws_lb.backend.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.backend.arn
  }
}

resource "aws_lb_listener" "backend_https" {
  count = var.backend_enable_https ? 1 : 0

  load_balancer_arn = aws_lb.backend.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = local.backend_certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.backend.arn
  }

  depends_on = [aws_acm_certificate_validation.backend]

  lifecycle {
    precondition {
      condition     = !var.backend_enable_https || var.backend_certificate_arn != "" || local.use_route53
      error_message = "Provide backend_certificate_arn or set route53_hosted_zone_id so Terraform can request and validate a backend ACM certificate."
    }
  }
}

resource "aws_ecs_task_definition" "backend" {
  family                   = "${local.project}-${local.env}-backend"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = tostring(var.backend_task_cpu)
  memory                   = tostring(var.backend_task_memory)
  execution_role_arn       = aws_iam_role.ecs_task_execution.arn
  task_role_arn            = aws_iam_role.backend_app.arn

  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }

  container_definitions = jsonencode([
    {
      name      = "wavis-backend"
      image     = "${aws_ecr_repository.backend.repository_url}:${var.backend_image_tag}"
      essential = true
      command = [
        "/bin/sh",
        "-lc",
        "export DATABASE_URL=\"postgres://${var.rds_master_username}:$${RDS_MASTER_PASSWORD}@${aws_db_instance.postgres.address}:${aws_db_instance.postgres.port}/${var.rds_db_name}\" && exec /usr/local/bin/wavis-backend"
      ]
      portMappings = [
        {
          containerPort = var.backend_container_port
          hostPort      = var.backend_container_port
          protocol      = "tcp"
        }
      ]
      environment = concat(
        [
          for key, value in local.config : {
            name  = key
            value = value
          }
        ],
        [
          {
            name  = "LIVEKIT_EC2_INSTANCE_ID"
            value = local.livekit_instance_id
          },
          {
            name  = "LIVEKIT_EC2_REGION"
            value = var.region
          }
        ]
      )
      secrets = [
        for key, parameter in data.aws_ssm_parameter.secrets : {
          name      = key
          valueFrom = parameter.arn
        }
        if contains(local.backend_secret_keys, key)
      ]
      healthCheck = {
        command     = ["CMD-SHELL", "curl -sf http://localhost:${var.backend_container_port}${var.backend_health_check_path} || exit 1"]
        interval    = 30
        timeout     = 5
        retries     = 3
        startPeriod = 30
      }
      logConfiguration = {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.backend.name
          awslogs-region        = var.region
          awslogs-stream-prefix = "ecs"
        }
      }
    }
  ])

  tags = local.tags
}

resource "aws_ecs_service" "backend" {
  name                              = "${local.project}-${local.env}-backend"
  cluster                           = aws_ecs_cluster.backend.id
  task_definition                   = aws_ecs_task_definition.backend.arn
  desired_count                     = var.backend_desired_count
  launch_type                       = "FARGATE"
  health_check_grace_period_seconds = var.backend_health_check_grace_period_seconds
  enable_execute_command            = true
  wait_for_steady_state             = false

  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }

  deployment_minimum_healthy_percent = 100
  deployment_maximum_percent         = 200

  network_configuration {
    subnets          = [for subnet in aws_subnet.private_app : subnet.id]
    security_groups  = [aws_security_group.backend.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.backend.arn
    container_name   = "wavis-backend"
    container_port   = var.backend_container_port
  }

  depends_on = [
    aws_lb_listener.backend_http,
    aws_lb_listener.backend_https,
  ]

  lifecycle {
    # CI owns mutable backend image/task rollouts in dev. Terraform should not
    # rewind the service to an older task revision during unrelated infra applies.
    ignore_changes = [task_definition]
  }

  tags = local.tags
}

resource "aws_appautoscaling_target" "backend" {
  count = var.backend_autoscaling_enabled ? 1 : 0

  max_capacity       = var.backend_autoscaling_max_capacity
  min_capacity       = var.backend_autoscaling_min_capacity
  resource_id        = "service/${aws_ecs_cluster.backend.name}/${aws_ecs_service.backend.name}"
  scalable_dimension = "ecs:service:DesiredCount"
  service_namespace  = "ecs"
}

resource "aws_appautoscaling_policy" "backend_cpu" {
  count = var.backend_autoscaling_enabled ? 1 : 0

  name               = "${local.project}-${local.env}-backend-cpu"
  policy_type        = "TargetTrackingScaling"
  resource_id        = aws_appautoscaling_target.backend[0].resource_id
  scalable_dimension = aws_appautoscaling_target.backend[0].scalable_dimension
  service_namespace  = aws_appautoscaling_target.backend[0].service_namespace

  target_tracking_scaling_policy_configuration {
    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageCPUUtilization"
    }

    target_value       = var.backend_autoscaling_cpu_target
    scale_in_cooldown  = 120
    scale_out_cooldown = 60
  }
}

resource "aws_appautoscaling_policy" "backend_memory" {
  count = var.backend_autoscaling_enabled ? 1 : 0

  name               = "${local.project}-${local.env}-backend-memory"
  policy_type        = "TargetTrackingScaling"
  resource_id        = aws_appautoscaling_target.backend[0].resource_id
  scalable_dimension = aws_appautoscaling_target.backend[0].scalable_dimension
  service_namespace  = aws_appautoscaling_target.backend[0].service_namespace

  target_tracking_scaling_policy_configuration {
    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageMemoryUtilization"
    }

    target_value       = var.backend_autoscaling_memory_target
    scale_in_cooldown  = 120
    scale_out_cooldown = 60
  }
}

resource "aws_cloudwatch_metric_alarm" "backend_cpu_high" {
  alarm_name          = "${local.project}-${local.env}-backend-cpu-high"
  alarm_description   = "Backend ECS service CPU utilization is high"
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 2
  metric_name         = "CPUUtilization"
  namespace           = "AWS/ECS"
  period              = 300
  statistic           = "Average"
  threshold           = var.backend_cpu_alarm_threshold
  alarm_actions       = [aws_sns_topic.alerts.arn]
  ok_actions          = [aws_sns_topic.alerts.arn]

  dimensions = {
    ClusterName = aws_ecs_cluster.backend.name
    ServiceName = aws_ecs_service.backend.name
  }

  tags = local.tags
}

resource "aws_cloudwatch_metric_alarm" "backend_unhealthy_targets" {
  alarm_name          = "${local.project}-${local.env}-backend-unhealthy-targets"
  alarm_description   = "Backend ALB target group has unhealthy targets"
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 2
  metric_name         = "UnHealthyHostCount"
  namespace           = "AWS/ApplicationELB"
  period              = 60
  statistic           = "Maximum"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = [aws_sns_topic.alerts.arn]
  ok_actions          = [aws_sns_topic.alerts.arn]

  dimensions = {
    LoadBalancer = aws_lb.backend.arn_suffix
    TargetGroup  = aws_lb_target_group.backend.arn_suffix
  }

  tags = local.tags
}
