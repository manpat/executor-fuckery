use std::future::{Future, poll_fn};
use std::task::{self, Poll};
use std::pin::Pin;

use std::cell::{Cell, UnsafeCell};
use std::collections::BinaryHeap;


slotmap::new_key_type! {
    pub struct TaskId;
}


pub struct Executor {
	tasks: slotmap::SlotMap<TaskId, Task>,
	queues: Box<UnsafeCell<Queues>>,
}

impl Executor {
	pub fn new() -> Executor {
		let queues = Box::new(UnsafeCell::new(Queues {
			update_queue: Vec::new(),
			trigger_queue: Vec::new(),
			timer_queue: BinaryHeap::new(),
			ticket_counter: 0,
		}));

		QUEUES.set(queues.get());

		Executor {
			tasks: slotmap::SlotMap::with_key(),
			queues,
		}
	}

	pub fn spawn(&mut self, f: impl Future<Output=()> + 'static) {
		let task = Task::new(f);
		let task_id = self.tasks.insert(task);
		self.queues.get_mut().update_queue.push(task_id);
	}

	pub fn active_tasks(&self) -> usize {
		self.tasks.len()
	}
}


impl Executor {
	pub fn poll(&mut self) {
		// Update timers
		let now = Instant::now();
		let Queues{ update_queue, timer_queue, ..  } = self.queues.get_mut();

		while let Some(soonest) = timer_queue.peek() {
			if now < soonest.deadline {
				break;
			}

			let Some(TimerEntry{task_id, ..}) = timer_queue.pop() else { unreachable!() };
			update_queue.push(task_id);
		}

		// No mutable references can be held to self.queues while polling futures
		let update_queue = std::mem::take(&mut self.queues.get_mut().update_queue);
		self.poll_ids(&update_queue);
	}

	pub fn trigger(&mut self) {
		let queues = self.queues.get_mut();
		queues.update_queue.append(&mut queues.trigger_queue);
	}

	fn poll_ids(&mut self, task_ids: &[TaskId]) {
		use std::task::Context;

		for &task_id in task_ids {
			let Some(task) = self.tasks.get_mut(task_id) else { continue };

			CURRENT_TASK.set(Some(task_id));

			match task.fut.as_mut().poll(&mut Context::from_waker(&noop_waker())) {
				Poll::Pending => {},
				Poll::Ready(()) => {
					self.tasks.remove(task_id);
				}
			}
		}

		CURRENT_TASK.set(None);
	}
}

impl Drop for Executor {
	fn drop(&mut self) {
		QUEUES.set(std::ptr::null_mut());
	}
}


thread_local! {
	static QUEUES: Cell<*mut Queues> = const { Cell::new(std::ptr::null_mut()) };
	static CURRENT_TASK: Cell<Option<TaskId>> = const { Cell::new(None) };
}


struct Queues {
	update_queue: Vec<TaskId>,
	trigger_queue: Vec<TaskId>,

	timer_queue: BinaryHeap<TimerEntry>,

	ticket_counter: usize,
}


use std::time::{Duration, Instant};
use std::cmp::{Ord, PartialOrd, Ordering};

#[derive(Eq, PartialEq)]
struct TimerEntry {
	deadline: Instant,
	task_id: TaskId,
	ticket: usize,
}


impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> Ordering {
    	// Sort by _soonest_ deadline, then by task_id, to stay consistent with Eq
        self.deadline.cmp(&other.deadline).reverse()
        	.then_with(|| self.task_id.cmp(&other.task_id))
    }
}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}




fn noop_raw_waker() -> task::RawWaker {
	fn noop(_: *const ()) {}

	task::RawWaker::new(
		std::ptr::null(),
		&task::RawWakerVTable::new(|_| noop_raw_waker(), noop, noop, noop)
	)
}

fn noop_waker() -> task::Waker {
	unsafe {
		task::Waker::from_raw(noop_raw_waker())
	}
}




struct Task {
	fut: Pin<Box<dyn Future<Output=()> + 'static>>,
}

impl Task {
	fn new(f: impl Future<Output=()> + 'static) -> Task {
		let fut = Box::pin(f);
		Task { fut }
	}
}



fn get_queues() -> &'static mut Queues {
	unsafe {
		let ptr = QUEUES.get().as_mut()
			.expect("Polling future from outside of Executor context!");

		&mut *ptr
	}
}


pub async fn next_update() {
	schedule_on_queue(|queues, task_id| {
		queues.update_queue.push(task_id);
		None
	}).await
}


pub async fn on_trigger() {
	schedule_on_queue(|queues, task_id| {
		queues.trigger_queue.push(task_id);
		None
	}).await
}

pub async fn timeout(duration: Duration) {
	schedule_on_queue(|queues, task_id| {
		let ticket = queues.ticket_counter;
		queues.ticket_counter = queues.ticket_counter.wrapping_add(1);

		queues.timer_queue.push(TimerEntry {
			deadline: Instant::now() + duration,
			task_id,
			ticket,
		});

		Some(ticket)
	}).await
}


fn schedule_on_queue<F>(f: F) -> ScheduleOnQueue<F>
	where F: FnOnce(&mut Queues, TaskId) -> Option<usize>
{
	ScheduleOnQueue(ScheduleOnQueueState::Pending(Some(f)))
}



struct ScheduleOnQueue<F>(ScheduleOnQueueState<F>);

enum ScheduleOnQueueState<F> {
	Pending(Option<F>),
	Scheduled(Option<usize>),
}


impl<F> Future for ScheduleOnQueue<F>
	where F: FnOnce(&mut Queues, TaskId) -> Option<usize>
{
	type Output = ();

	fn poll(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> Poll<()> {
		let state = unsafe{ &mut self.get_unchecked_mut().0 };

		match state {
			ScheduleOnQueueState::Pending(f) => {
				let current_task = CURRENT_TASK.get().unwrap();
				let ticket = f.take().unwrap()(get_queues(), current_task);
				*state = ScheduleOnQueueState::Scheduled(ticket);
				Poll::Pending
			}

			ScheduleOnQueueState::Scheduled(_) => Poll::Ready(())
		}
	}
}

impl<F> Drop for ScheduleOnQueue<F> {
	fn drop(&mut self) {
		inner_drop(unsafe { Pin::new_unchecked(self)});
        fn inner_drop<F>(this: Pin<&mut ScheduleOnQueue<F>>) {
            if let ScheduleOnQueueState::Scheduled(Some(ticket)) = this.0 {
            	// TODO(pat.m): generic tickets
            	get_queues().timer_queue.retain(move |entry| entry.ticket != ticket);
            }
        }
	}
}



pub async fn when_first<A, B>(mut a: impl Future<Output=A>, mut b: impl Future<Output=B>) {
	poll_fn(|cx| {
		let a = unsafe { Pin::new_unchecked(&mut a) };
		let b = unsafe { Pin::new_unchecked(&mut b) };

		match a.poll(cx) {
			Poll::Pending => {}
			Poll::Ready(_) => {
				return Poll::Ready(())
			}
		}

		match b.poll(cx) {
			Poll::Pending => {}
			Poll::Ready(_) => {
				return Poll::Ready(())
			}
		}

		Poll::Pending
	}).await;
}