from manim import RIGHT, Circle, Dot, Scene, Square, VGroup


def spin(mob, dt):
    mob.rotate(dt)


class ReAdd(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        self.add(a, b)
        self.add(a)
        self.play(a.animate.shift(RIGHT))


class Grouped(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        group = VGroup(a, b)
        extra = Dot()
        self.add(group, extra)
        self.play(group.animate.shift(RIGHT))


class UpdaterFront(Scene):
    def construct(self):
        background = Square()
        first = Circle()
        second = Dot()
        self.add(background, first, second)
        background.add_updater(spin)
        self.wait()


class ForegroundScene(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        self.add(a, b)
        self.add_foreground_mobject(b)
        self.wait()


class Branchy(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        if a.width > 1:
            self.add(a)
        self.add(b)
        self.wait()
