<script lang="ts">
	import { FileText, ListTodo, BarChart3, Video, Database, Settings } from '@lucide/svelte';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import { useSidebar } from '$lib/components/ui/sidebar/context.svelte.js';
	import { clearAll } from '$lib/stores/filter';
	import { goto } from '$app/navigation';

	const sidebar = useSidebar();

	function handleLogoClick() {
		clearAll();
		goto('/');

		if (sidebar.isMobile) {
			sidebar.setOpenMobile(false);
		}
	}

	function handleNavClick(href: string) {
		goto(href);
		if (sidebar.isMobile) {
			sidebar.setOpenMobile(false);
		}
	}
</script>

<Sidebar.Root class="border-border bg-background border-r">
	<Sidebar.Header class="border-border flex h-[73px] items-center border-b">
		<a
			href="/"
			class="flex w-full items-center gap-3 px-4 py-3 hover:cursor-pointer"
			on:click={handleLogoClick}
		>
			<div class="flex h-8 w-8 items-center justify-center overflow-hidden rounded-lg">
				<img src="/favicon.png" alt="Bili Sync-up" class="h-6 w-6" />
			</div>
			<div class="grid flex-1 text-left text-sm leading-tight">
				<span class="truncate font-semibold">Bili Sync-up</span>
				<span class="text-muted-foreground truncate text-xs">视频管理系统</span>
			</div>
		</a>
	</Sidebar.Header>
	<Sidebar.Content class="flex flex-col px-2 py-3">
		<div class="flex-1 space-y-6">
			<!-- 总览 -->
			<div>
				<div class="text-muted-foreground mb-2 px-3 text-xs font-medium tracking-wider uppercase">
					总览
				</div>
				<Sidebar.Menu class="space-y-1">
					<Sidebar.MenuItem>
						<Sidebar.MenuButton>
							<button
								class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
								on:click={() => handleNavClick('/')}
							>
								<div class="flex flex-1 items-center gap-3">
									<BarChart3 class="text-muted-foreground h-4 w-4" />
									<span class="text-sm">仪表盘</span>
								</div>
							</button>
						</Sidebar.MenuButton>
					</Sidebar.MenuItem>
					<Sidebar.MenuItem>
						<Sidebar.MenuButton>
							<button
								class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
								on:click={() => handleNavClick('/logs')}
							>
								<div class="flex flex-1 items-center gap-3">
									<FileText class="text-muted-foreground h-4 w-4" />
									<span class="text-sm">日志</span>
								</div>
							</button>
						</Sidebar.MenuButton>
					</Sidebar.MenuItem>
				</Sidebar.Menu>
			</div>

			<!-- 内容管理 -->
			<div>
				<div class="text-muted-foreground mb-2 px-3 text-xs font-medium tracking-wider uppercase">
					内容管理
				</div>
				<Sidebar.Menu class="space-y-1">
					<Sidebar.MenuItem>
						<Sidebar.MenuButton>
							<button
								class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
								on:click={() => handleNavClick('/videos')}
							>
								<div class="flex flex-1 items-center gap-3">
									<Video class="text-muted-foreground h-4 w-4" />
									<span class="text-sm">视频</span>
								</div>
							</button>
						</Sidebar.MenuButton>
					</Sidebar.MenuItem>
					<Sidebar.MenuItem>
						<Sidebar.MenuButton>
							<button
								class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
								on:click={() => handleNavClick('/video-sources')}
							>
								<div class="flex flex-1 items-center gap-3">
									<Database class="text-muted-foreground h-4 w-4" />
									<span class="text-sm">视频源</span>
								</div>
							</button>
						</Sidebar.MenuButton>
					</Sidebar.MenuItem>
				</Sidebar.Menu>
			</div>
		</div>

		<!-- 底部系统功能 -->
		<div class="border-border mt-auto border-t pt-4">
			<Sidebar.Menu class="space-y-1">
				<Sidebar.MenuItem>
					<Sidebar.MenuButton>
						<button
							class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
							on:click={() => handleNavClick('/queue')}
						>
							<div class="flex flex-1 items-center gap-3">
								<ListTodo class="text-muted-foreground h-4 w-4" />
								<span class="text-sm">任务队列</span>
							</div>
						</button>
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
				<Sidebar.MenuItem>
					<Sidebar.MenuButton>
						<button
							class="hover:bg-accent/50 text-foreground flex w-full cursor-pointer items-center rounded-lg px-3 py-2.5 font-medium transition-all duration-200"
							on:click={() => handleNavClick('/settings')}
						>
							<div class="flex flex-1 items-center gap-3">
								<Settings class="text-muted-foreground h-4 w-4" />
								<span class="text-sm">设置</span>
							</div>
						</button>
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
			</Sidebar.Menu>
		</div>
	</Sidebar.Content>
</Sidebar.Root>
