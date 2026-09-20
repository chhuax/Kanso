/**
 * Everyday invocations of the tools people live in, offered in the completion
 * popup after the user's own history: a shell that has never run `kubectl`
 * still knows how a port-forward is spelled.
 *
 * Two rules for what goes in here. It has to be worth remembering — the
 * spelling of a flag, the order of arguments — and it has to be safe to run
 * by accident: nothing that deletes, drains, prunes or force-pushes, so a
 * stray Enter costs a look at a pod list rather than a cluster.
 *
 * Entries ending in a space are the ones that take an argument, so picking
 * one leaves the cursor where the name goes.
 */
export const COMMON_COMMANDS: string[] = [
  // kubectl
  "kubectl get pods -A",
  "kubectl get pods -o wide",
  "kubectl get pods -A -o wide",
  "kubectl get pods --field-selector status.phase!=Running",
  "kubectl get svc",
  "kubectl get deploy",
  "kubectl get sts",
  "kubectl get ds",
  "kubectl get nodes -o wide",
  "kubectl get all",
  "kubectl get events --sort-by=.lastTimestamp",
  "kubectl describe pod ",
  "kubectl describe node ",
  "kubectl logs -f ",
  "kubectl logs --tail=200 ",
  "kubectl logs -f --previous ",
  "kubectl exec -it  -- sh",
  "kubectl exec -it  -- bash",
  "kubectl top pods",
  "kubectl top nodes",
  "kubectl config get-contexts",
  "kubectl config current-context",
  "kubectl config use-context ",
  "kubectl -n kube-system get pods",
  "kubectl rollout status deploy/ ",
  "kubectl rollout history deploy/ ",
  "kubectl api-resources | grep ",
  "kubectl explain pod.spec.containers",
  "kubectl auth can-i --list",
  "kubectl apply -f ",
];

/** docker, including the compose subcommands people reach for most. */
export const DOCKER_COMMANDS: string[] = [
  "docker ps",
  "docker ps -a",
  "docker ps --format 'table {{.Names}}\\t{{.Status}}\\t{{.Ports}}'",
  "docker images",
  "docker logs -f ",
  "docker logs --tail=200 ",
  "docker exec -it  sh",
  "docker exec -it  bash",
  "docker inspect ",
  "docker stats",
  "docker port ",
  "docker network ls",
  "docker volume ls",
  "docker system df",
  "docker compose ps",
  "docker compose up -d",
  "docker compose logs -f",
  "docker compose restart ",
  "docker compose config",
  "docker build -t  .",
  "docker run --rm -it  sh",
];

/** git, minus anything that rewrites somebody else's history. */
export const GIT_COMMANDS: string[] = [
  "git status",
  "git log --oneline -20",
  "git log --graph --oneline --all",
  "git log --stat -5",
  "git diff",
  "git diff --staged",
  "git diff --stat",
  "git add -p",
  "git add -A",
  "git commit --amend --no-edit",
  "git pull --rebase",
  "git fetch --prune",
  "git push --force-with-lease",
  "git switch -c ",
  "git switch ",
  "git branch -vv",
  "git branch --merged",
  "git remote -v",
  "git stash",
  "git stash pop",
  "git stash list",
  "git restore --staged ",
  "git restore ",
  "git clean -nd",
  "git rebase -i HEAD~",
  "git cherry-pick ",
  "git blame ",
  "git show --stat HEAD",
  "git tag --sort=-creatordate | head",
];

/** systemd and journalctl: what is running, and why it is not. */
export const SYSTEMD_COMMANDS: string[] = [
  "systemctl status ",
  "systemctl --failed",
  "systemctl list-units --type=service --state=running",
  "systemctl list-timers",
  "systemctl cat ",
  "systemctl show -p FragmentPath ",
  "systemctl restart ",
  "systemctl reload ",
  "systemctl enable --now ",
  "journalctl -u  -f",
  "journalctl -u  --since '1 hour ago'",
  "journalctl -u  --since today",
  "journalctl -p err -b",
  "journalctl -xe",
  "journalctl --disk-usage",
];

/** Every shipped command, in the order the groups are read. */
export const COMMON_COMMAND_GROUPS: string[][] = [
  COMMON_COMMANDS,
  DOCKER_COMMANDS,
  GIT_COMMANDS,
  SYSTEMD_COMMANDS,
];
