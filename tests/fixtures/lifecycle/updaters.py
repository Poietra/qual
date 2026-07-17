from manim import Scene, Square


def spin(mob, dt):
    mob.rotate(dt)


def follow(mob):
    mob.shift(1)


class Updaters(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.add_updater(spin)
        sq.add_updater(follow)
        sq.remove_updater(spin)
        sq.remove_updater(lambda m: m)
        self.wait()


class DynamicWait(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.add_updater(spin)
        self.wait()
