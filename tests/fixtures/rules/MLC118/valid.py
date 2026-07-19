from manim import BLUE, ORIGIN, RED, RIGHT, Scene, Square


class ExplicitNoSuspend(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda mob: mob.move_to(ORIGIN))
        self.add(square)
        self.play(square.animate.shift(RIGHT), suspend_mobject_updating=False)


class DisjointChannels(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda mob: mob.set_color(RED))
        self.add(square)
        self.play(square.animate.shift(RIGHT))


class RemovedUpdater(Scene):
    def construct(self):
        square = Square()
        updater = lambda mob: mob.move_to(ORIGIN)
        square.add_updater(updater)
        square.remove_updater(updater)
        self.add(square)
        self.play(square.animate.shift(RIGHT))


class ConditionalWrite(Scene):
    def construct(self, condition):
        square = Square()

        def updater(mob):
            if condition:
                mob.move_to(ORIGIN)

        square.add_updater(updater)
        self.add(square)
        self.play(square.animate.shift(RIGHT))
