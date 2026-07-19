from manim import Dot as D
from manim import RIGHT as R
from manim import Scene as S


class AliasedLag(S):
    def construct(self):
        follower = D()
        driver = D()
        follower.add_updater(lambda mob: mob.move_to(driver.get_center()))
        driver.add_updater(lambda mob, dt: mob.shift(R * dt))
        self.add(follower, driver)
        self.wait(2)
